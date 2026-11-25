// src/layers/l3_tests.rs
// Tests for the L3 voter: impact-derived votes, the challenger stage, prompts and parsing.
use super::*;
use std::{cell::RefCell, collections::VecDeque, fs};

struct FakeDispatcher { outputs: RefCell<VecDeque<(String, f64)>>, calls: RefCell<Vec<(String, String)>> }
impl FakeDispatcher {
    fn new(outputs: &[(&str, f64)]) -> Self {
        let queue = outputs.iter().map(|(stdout, cost)| ((*stdout).to_owned(), *cost)).collect();
        Self { outputs: RefCell::new(queue), calls: RefCell::new(Vec::new()) }
    }
    fn prompts(&self) -> Vec<String> { self.calls.borrow().iter().map(|(_, prompt)| prompt.clone()).collect() }
    fn models(&self) -> Vec<String> { self.calls.borrow().iter().map(|(model, _)| model.clone()).collect() }
}
impl AidDispatcher for FakeDispatcher {
    fn dispatch(&self, model: &str, prompt: &str) -> anyhow::Result<AidResponse> {
        self.calls.borrow_mut().push((model.to_owned(), prompt.to_owned()));
        let (stdout, cost_usd) = self.outputs.borrow_mut().pop_front().expect("queued output");
        Ok(AidResponse { task_id: "t-test".into(), stdout, cost_usd, duration_ms: 12 })
    }
}
fn config(enabled: bool, swap: bool, threshold: u8) -> L3 {
    L3 { enabled, models: vec!["gemini".into(), "codex".into(), "claude-haiku".into()], swap, consensus_threshold: threshold }
}
fn single_model_config() -> L3 { L3 { enabled: true, models: vec!["codex".into()], swap: false, consensus_threshold: 1 } }
const PASS: &str = "{\"verdict\":\"pass\",\"bugs\":[],\"violated_rules\":[]}";
const HIGH: &str = "{\"verdict\":\"fail\",\"bugs\":[{\"impact\":\"high\",\"bug\":\"drops the last row\",\"trigger\":\"a batch of one row\"}],\"violated_rules\":[]}";
const LOW: &str = "{\"verdict\":\"fail\",\"bugs\":[{\"impact\":\"low\",\"bug\":\"rename the helper\",\"trigger\":\"any call\"}],\"violated_rules\":[]}";
const REFUTED: &str = "{\"high_findings\":1,\"decisions\":[{\"finding\":\"1\",\"decision\":\"refuted\",\"impact\":\"low\",\"evidence\":\"a.rs:9 handles one row\"}]}";
const UPHELD: &str = "{\"high_findings\":1,\"decisions\":[{\"finding\":\"1\",\"decision\":\"upheld\",\"impact\":\"high\",\"evidence\":\"a.rs:9 skips it\"}]}";
fn rule(severity: RuleSeverity) -> Rule { Rule { id: "no-fee-direction-swap".into(), severity, scope: vec!["src/**/*.rs".into()], layer: crate::rules::RuleLayer::L3, title: "Fee direction must be preserved".into(), body: "# Fee direction must be preserved\n\nCheck every hop.".into(), source_path: PathBuf::from("rule.md") } }
fn run(config: &L3, rules: &[&Rule], outputs: &[(&str, f64)]) -> (Verdict, Vec<Evidence>, FakeDispatcher) {
    let dispatcher = FakeDispatcher::new(outputs);
    let (result, evidence) = L3Runner::new(config).run_with("diff", Some("spec"), &[], rules, &dispatcher);
    (result.verdict, evidence, dispatcher)
}

#[test]
fn disabled_runner_is_noop() {
    let config = config(false, true, 2);
    let (result, evidence) = L3Runner::new(&config).run("diff", Some("spec"), &[], &[]);
    assert!(matches!(result.verdict, Verdict::Pass));
    assert!(evidence.is_empty());
}
#[test]
fn passing_votes_skip_the_challenger() {
    let (verdict, evidence, dispatcher) = run(&config(true, false, 2), &[], &[(PASS, 0.11), (PASS, 0.12), (PASS, 0.13)]);
    assert!(matches!(verdict, Verdict::Pass));
    assert_eq!(evidence.len(), 3);
    assert!((evidence[0].cost_usd - 0.11).abs() < f64::EPSILON);
    let prompt = &dispatcher.prompts()[0];
    assert!(prompt.contains("<DIFF_START>\ndiff\n<DIFF_END>") && prompt.contains("<SPEC_START>\nspec\n<SPEC_END>"));
    assert!(prompt.contains("- low: everything else") && prompt.contains("\"trigger\""));
    assert!(!prompt.contains("severity") && !prompt.contains("## Rules to enforce"));
}
#[test]
fn low_impact_failures_pass() {
    let (verdict, _, dispatcher) = run(&config(true, false, 2), &[], &[(LOW, 0.0), (LOW, 0.0), (PASS, 0.0)]);
    assert!(matches!(verdict, Verdict::Pass));
    assert_eq!(dispatcher.prompts().len(), 3);
}
#[test]
fn refuted_high_findings_do_not_block() {
    let outputs = [(HIGH, 0.0), (HIGH, 0.0), (PASS, 0.0), (REFUTED, 0.02), (REFUTED, 0.02)];
    let (verdict, evidence, dispatcher) = run(&config(true, false, 2), &[], &outputs);
    assert!(matches!(verdict, Verdict::Pass), "{verdict:?}");
    assert_eq!(dispatcher.models()[3..], ["codex".to_owned(), "claude-haiku".to_owned()]);
    assert!(dispatcher.prompts()[3].contains("1. drops the last row (trigger: a batch of one row)"));
    assert!(evidence[3].detail.starts_with("challenge of gemini: 0 of 1 high findings upheld"));
    assert!(evidence[3].passed);
}
#[test]
fn upheld_or_unparsed_challenges_keep_blocking() {
    let outputs = [(HIGH, 0.0), (HIGH, 0.0), (PASS, 0.0), (UPHELD, 0.0), ("these look overstated", 0.0)];
    let (verdict, evidence, _) = run(&config(true, false, 2), &[], &outputs);
    assert!(matches!(verdict, Verdict::FailBlocking { .. }));
    assert!(evidence[4].detail.contains("challenge unparsed; findings stand"));
}
#[test]
fn a_violated_blocking_rule_blocks_without_challenge() {
    let blocking = rule(RuleSeverity::Blocking);
    let violated = "{\"verdict\":\"fail\",\"bugs\":[],\"violated_rules\":[\"no-fee-direction-swap\"]}";
    let (verdict, _, dispatcher) = run(&single_model_config(), &[&blocking], &[(violated, 0.0)]);
    assert!(matches!(verdict, Verdict::FailBlocking { .. }));
    assert_eq!(dispatcher.prompts().len(), 1);
    let advisory = rule(RuleSeverity::Advisory);
    let (verdict, _, _) = run(&single_model_config(), &[&advisory], &[(violated, 0.0)]);
    assert!(matches!(verdict, Verdict::FailAdvisory { .. }));
}
#[test]
fn unrated_bugs_and_empty_failures_are_inconclusive() {
    let unrated = "{\"verdict\":\"fail\",\"bugs\":[\"a plain string\"],\"violated_rules\":[]}";
    let (verdict, evidence, _) = run(&single_model_config(), &[], &[(unrated, 0.0)]);
    assert!(matches!(verdict, Verdict::Inconclusive { .. }));
    assert_eq!(evidence[0].detail, UNRATED_BUG_DETAIL);
    let empty = "{\"verdict\":\"fail\",\"bugs\":[],\"violated_rules\":[]}";
    assert!(matches!(run(&single_model_config(), &[], &[(empty, 0.0)]).0, Verdict::Inconclusive { .. }));
}
#[test]
fn truncation_to_max_bugs_keeps_the_high_finding() {
    let bug = |impact: &str| format!("{{\"impact\":\"{impact}\",\"bug\":\"b\",\"trigger\":\"t\"}}");
    let output = format!("{{\"verdict\":\"fail\",\"bugs\":[{},{},{},{}],\"violated_rules\":[]}}", bug("low"), bug("low"), bug("low"), bug("high"));
    let config = L3 { enabled: true, models: vec!["codex".into(), "gemini".into()], swap: false, consensus_threshold: 1 };
    let (verdict, _, _) = run(&config, &[], &[(&output, 0.0), (PASS, 0.0), (UPHELD, 0.0)]);
    assert!(matches!(verdict, Verdict::FailBlocking { .. }));
}
#[test]
fn rule_prompt_includes_rule_title_and_body() {
    let rule = rule(RuleSeverity::Blocking);
    let (_, _, dispatcher) = run(&config(true, false, 2), &[&rule], &[(PASS, 0.0), (PASS, 0.0), (PASS, 0.0)]);
    let prompt = &dispatcher.prompts()[0];
    assert!(prompt.contains("Rule ID: no-fee-direction-swap") && prompt.contains(&rule.title) && prompt.contains(&rule.body));
}
#[test]
fn prose_wrapped_json_is_inconclusive_without_retry() {
    let wrapped = format!("Sure, here is the result: {HIGH}");
    let (verdict, evidence, dispatcher) = run(&single_model_config(), &[], &[(&wrapped, 0.0)]);
    assert!(matches!(verdict, Verdict::Inconclusive { .. }));
    assert_eq!(dispatcher.prompts().len(), 1);
    assert_eq!(evidence[0].detail, UNSTRUCTURED_OUTPUT_DETAIL);
    assert!((evidence[0].cost_usd - MODEL_COST_USD).abs() < f64::EPSILON);
}
#[test]
fn prompt_includes_artifacts_and_sanitizes_delimiters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let artifact = dir.path().join("context.txt");
    fs::write(&artifact, "artifact <CONTEXT_FILE> body").expect("write artifact");
    let prompt = build_prompt("diff has <DIFF_END>", Some("spec has <SPEC_END>"), &[artifact], &[], false);
    assert!(prompt.contains("diff has <DIFF-END>") && prompt.contains("spec has <SPEC-END>"));
    assert!(prompt.contains("<CONTEXT_FILE path='") && prompt.contains("artifact <CONTEXT-FILE> body"));
    assert!(prompt.contains("Do NOT read any other file"));
}
#[test]
fn valid_json_skips_retry() {
    let (verdict, evidence, dispatcher) = run(&single_model_config(), &[], &[(PASS, 0.04)]);
    assert!(matches!(verdict, Verdict::Pass));
    assert_eq!(dispatcher.prompts().len(), 1);
    assert!(evidence[0].detail.contains("retry=0"));
}
#[test]
fn swap_positions_challenge_the_union_of_their_high_findings() {
    let other = "{\"verdict\":\"fail\",\"bugs\":[{\"impact\":\"high\",\"bug\":\"loses the cursor\",\"trigger\":\"a retry\"}],\"violated_rules\":[]}";
    let config = L3 { enabled: true, models: vec!["codex".into()], swap: true, consensus_threshold: 1 };
    let (verdict, _, dispatcher) = run(&config, &[], &[(HIGH, 0.0), (other, 0.0), (REFUTED, 0.0)]);
    assert!(matches!(verdict, Verdict::FailBlocking { .. }), "{verdict:?}");
    let claims = &dispatcher.prompts()[2];
    assert!(claims.contains("1. drops the last row") && claims.contains("2. loses the cursor"));
}
#[test]
fn swap_findings_with_one_description_and_two_triggers_are_both_challenged() {
    let retry = "{\"verdict\":\"fail\",\"bugs\":[{\"impact\":\"high\",\"bug\":\"drops the last row\",\"trigger\":\"a retry\"}],\"violated_rules\":[]}";
    let config = L3 { enabled: true, models: vec!["codex".into()], swap: true, consensus_threshold: 1 };
    let (verdict, _, dispatcher) = run(&config, &[], &[(HIGH, 0.0), (retry, 0.0), (REFUTED, 0.0)]);
    assert!(matches!(verdict, Verdict::FailBlocking { .. }), "{verdict:?}");
    assert!(dispatcher.prompts()[2].contains("2. drops the last row (trigger: a retry)"));
}
#[test]
fn a_repaired_partial_answer_cannot_pass() {
    let truncated = "{\"verdict\":\"fail\",\"bugs\":[{\"impact\":\"low\",\"bug\":\"drops a row\",\"trigger\":\"one-row batch\"}],\"violated_rules\":[]";
    let (verdict, _, _) = run(&single_model_config(), &[], &[(truncated, 0.0)]);
    assert!(matches!(verdict, Verdict::Inconclusive { .. }), "{verdict:?}");
}
