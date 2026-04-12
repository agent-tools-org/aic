// src/layers/l3.rs
// L3 cross-LLM voter. Exports: L3Runner; deps: config, types, serde_json, std::process.
use crate::config::L3;
use crate::layers::aid_dispatch::{AidDispatcher, AidResponse, RealAidDispatcher};
use crate::layers::{render_artifact_context, sanitize_for_delimiters};
use crate::layers::json_extract::{extract_json_object, repair_partial_json, SchemaHint};
use crate::rules::Rule;
use crate::types::{Evidence, Layer, LayerResult, SignalKind, Verdict};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;
const PROMPT_DELIMITERS: &[&str] = &["<DIFF_START>", "<DIFF_END>", "<CONTEXT_FILE>", "</CONTEXT_FILE>", "<SPEC_START>", "<SPEC_END>"];
pub const MODEL_COST_USD: f64 = 0.05;
const MAX_BUGS: usize = 3;
const RETRY_SYSTEM_PROMPT: &str = "CRITICAL: Your previous response contained no JSON object. You MUST respond with ONLY a JSON object matching this schema: {verdict, bugs, severity, violated_rules}. Start with { and end with }. No reasoning, no preamble, no tool calls. This is a structured-output API, not an interactive chat.";
pub struct L3Runner<'cfg> { pub config: &'cfg L3 }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoteKind { Pass, FailBlocking, FailAdvisory, Inconclusive }
#[derive(Debug, Clone)]
struct CallOutcome { evidence: Evidence, vote: VoteKind }
#[derive(Debug, Deserialize)]
struct ReviewResponse { verdict: String, #[serde(default)] bugs: Vec<Value>, #[serde(default)] severity: String, #[serde(default)] violated_rules: Vec<String> }
enum ParseStatus { Parsed(ReviewResponse), Retry(String), Inconclusive(String) }
impl<'cfg> L3Runner<'cfg> {
    pub fn new(config: &'cfg L3) -> Self { Self { config } }
    pub fn run(&self, diff_text: &str, spec: Option<&str>, artifacts: &[PathBuf], rules: &[&Rule]) -> (LayerResult, Vec<Evidence>) {
        self.run_with(diff_text, spec, artifacts, rules, &RealAidDispatcher)
    }
    pub(crate) fn run_with<D>(&self, diff_text: &str, spec: Option<&str>, artifacts: &[PathBuf], rules: &[&Rule], dispatcher: &D) -> (LayerResult, Vec<Evidence>) where D: AidDispatcher + ?Sized {
        let started_at = Instant::now();
        if !self.config.enabled {
            return (layer_result(Verdict::Pass, 0, started_at.elapsed().as_millis() as u64), Vec::new());
        }
        let evaluation = evaluate_models(self.config, diff_text, spec, artifacts, rules, dispatcher);
        let verdict = aggregate_votes(self.config.consensus_threshold, &evaluation.votes);
        let evidence = evaluation.evidence;
        (layer_result(verdict, evidence.len(), started_at.elapsed().as_millis() as u64), evidence)
    }
}
struct Evaluation { evidence: Vec<Evidence>, votes: Vec<VoteKind> }
fn evaluate_models<D>(config: &L3, diff_text: &str, spec: Option<&str>, artifacts: &[PathBuf], rules: &[&Rule], dispatcher: &D) -> Evaluation where D: AidDispatcher + ?Sized {
    let mut evidence = Vec::new();
    let mut votes = Vec::new();
    let prompt = build_prompt(diff_text, spec, artifacts, rules, false);
    let swapped_prompt = config.swap.then(|| build_prompt(diff_text, spec, artifacts, rules, true));
    for model in &config.models {
        let normal = dispatch_review(dispatcher, model, &prompt);
        evidence.push(normal.evidence);
        let vote = if config.swap {
            let swapped = dispatch_review(dispatcher, model, swapped_prompt.as_deref().unwrap_or(&prompt));
            let final_vote = if normal.vote == swapped.vote { normal.vote } else { VoteKind::Inconclusive };
            evidence.push(swapped.evidence);
            final_vote
        } else {
            normal.vote
        };
        votes.push(vote);
    }
    Evaluation { evidence, votes }
}
fn dispatch_review<D>(dispatcher: &D, model: &str, prompt: &str) -> CallOutcome where D: AidDispatcher + ?Sized {
    match dispatcher.dispatch(model, prompt) {
        Ok(response) => {
            let output = response.stdout.trim().to_owned();
            let cost_usd = response_cost(&response);
            match parse_output(&output) {
                ParseStatus::Parsed(review) => parsed_outcome(model, &output, review, cost_usd, 0),
                ParseStatus::Inconclusive(detail) => inconclusive_outcome(model, detail, cost_usd),
                ParseStatus::Retry(_) => match dispatcher.dispatch(model, &retry_prompt(prompt)) {
                    Ok(retry_response) => {
                        let retry_output = retry_response.stdout.trim().to_owned();
                        let retry_cost_usd = cost_usd + response_cost(&retry_response);
                        match parse_output(&retry_output) {
                            ParseStatus::Parsed(review) => parsed_outcome(model, &retry_output, review, retry_cost_usd, 1),
                            ParseStatus::Retry(detail) | ParseStatus::Inconclusive(detail) => {
                                inconclusive_outcome(model, format!("retry=1; retry_also_failed; {detail}"), retry_cost_usd)
                            }
                        }
                    }
                    Err(error) => inconclusive_outcome(model, format!("retry=1; retry_also_failed; dispatch error: {error}"), cost_usd),
                },
            }
        }
        Err(error) => inconclusive_outcome(model, format!("dispatch error: {error}"), 0.0),
    }
}

fn parse_output(output: &str) -> ParseStatus {
    if let Some(json) = extract_json_object(output) {
        return match serde_json::from_str::<ReviewResponse>(json).map(normalize_response) {
            Ok(review) => ParseStatus::Parsed(review),
            Err(error) => repair_review(output).unwrap_or_else(|| ParseStatus::Retry(format!("invalid JSON: {error}; raw={json}"))),
        };
    }
    repair_review(output).unwrap_or_else(|| ParseStatus::Retry(format!("no JSON object found in model output: {output}")))
}
fn normalize_response(mut response: ReviewResponse) -> ReviewResponse {
    response.bugs.truncate(MAX_BUGS);
    response.violated_rules.retain(|id| !id.trim().is_empty());
    response.violated_rules.sort();
    response.violated_rules.dedup();
    response
}
fn classify_vote(response: &ReviewResponse) -> VoteKind {
    match response.verdict.as_str() {
        "pass" => VoteKind::Pass,
        "fail" => match response.severity.as_str() {
            "blocking" => VoteKind::FailBlocking,
            "advisory" => VoteKind::FailAdvisory,
            _ => VoteKind::Inconclusive,
        },
        _ => VoteKind::Inconclusive,
    }
}
fn inconclusive_outcome(model: &str, detail: String, cost_usd: f64) -> CallOutcome {
    CallOutcome {
        vote: VoteKind::Inconclusive,
        evidence: Evidence {
            layer: Layer::L3CrossLlm,
            signal: SignalKind::LlmVote,
            passed: false,
            detail,
            cite: Some(model.to_owned()),
            cost_usd,
        },
    }
}
fn repair_review(output: &str) -> Option<ParseStatus> {
    let repaired = repair_partial_json(output, SchemaHint::L3)?;
    Some(serde_json::from_str::<ReviewResponse>(&repaired).map(normalize_response).map(ParseStatus::Parsed).unwrap_or_else(|error| ParseStatus::Inconclusive(format!("invalid JSON: {error}; raw={repaired}"))))
}
fn parsed_outcome(model: &str, output: &str, response: ReviewResponse, cost_usd: f64, retry_count: u8) -> CallOutcome {
    CallOutcome { vote: classify_vote(&response), evidence: Evidence { layer: Layer::L3CrossLlm, signal: SignalKind::LlmVote, passed: response.verdict == "pass", detail: format_review_detail(output, &response, retry_count), cite: Some(model.to_owned()), cost_usd } }
}
fn response_cost(response: &AidResponse) -> f64 { if response.cost_usd == 0.0 { MODEL_COST_USD } else { response.cost_usd } }
fn retry_prompt(prompt: &str) -> String { format!("[SYSTEM]\n{RETRY_SYSTEM_PROMPT}\n\n{prompt}") }
fn format_review_detail(output: &str, response: &ReviewResponse, retry_count: u8) -> String {
    let violated_rules = if response.violated_rules.is_empty() { "[]".to_owned() } else { format!("[{}]", response.violated_rules.join(", ")) };
    format!("violated_rules={violated_rules}; retry={retry_count}; response={output}")
}
fn build_prompt(diff_text: &str, spec: Option<&str>, artifacts: &[PathBuf], rules: &[&Rule], swapped: bool) -> String {
    let spec_text = sanitize_for_delimiters(spec.unwrap_or("<none>"), PROMPT_DELIMITERS);
    let diff_text = sanitize_for_delimiters(diff_text, PROMPT_DELIMITERS);
    let artifacts = render_artifact_context(artifacts, PROMPT_DELIMITERS);
    let task = match (rules.is_empty(), swapped) {
        (true, _) => "Review this change for bugs.",
        (false, true) => "Review this change for bugs. The listed rules are additional review context.",
        (false, false) => "Review this change for bugs and enforce every listed rule.",
    };
    let rules_text = (!rules.is_empty()).then(|| rules.iter().map(|rule| format!("Rule ID: {}\nRule Title: {}\n<RULE_START>\n{}\n<RULE_END>", rule.id, rule.title, rule.body)).collect::<Vec<_>>().join("\n\n"));
    let ordered_sections = if swapped {
        format!("## Spec\n<SPEC_START>\n{spec_text}\n<SPEC_END>\n\n## Diff\n<DIFF_START>\n{diff_text}\n<DIFF_END>{artifacts}")
    } else {
        format!("## Diff\n<DIFF_START>\n{diff_text}\n<DIFF_END>{artifacts}\n\n## Spec\n<SPEC_START>\n{spec_text}\n<SPEC_END>")
    };
    format!(
        "[SYSTEM]\nYou are a code review JSON API. You respond ONLY with a single JSON object.\nYou NEVER include reasoning, explanation, preamble, postamble, or markdown fences.\nYour entire response must be a valid JSON object parseable by serde_json.\n\nRequired schema (all fields mandatory):\n{{\"verdict\":\"pass\"|\"fail\",\"bugs\":[\"<one-line description>\",\"...\"],\"severity\":\"blocking\"|\"advisory\",\"violated_rules\":[\"id\",\"...\"]}}\n\nExample response:\n{{\"verdict\":\"pass\",\"bugs\":[],\"severity\":\"advisory\",\"violated_rules\":[]}}\n\n[TASK]\n{task}{rules_section}\n\n{ordered_sections}\n\n[RESPONSE]\nRespond now with ONLY the JSON object. Your entire response must begin with {{ and end with }}.",
        task = task,
        rules_section = rules_text.map(|text| format!("\n\n## Rules to enforce\n{text}")).unwrap_or_default(),
    )
}
fn aggregate_votes(threshold: u8, votes: &[VoteKind]) -> Verdict {
    let threshold = usize::from(threshold);
    let blocking = votes.iter().filter(|vote| **vote == VoteKind::FailBlocking).count();
    if blocking >= threshold {
        return Verdict::FailBlocking { reason: format!("L3 blocking consensus: {blocking}") };
    }
    let advisory = votes.iter().filter(|vote| **vote == VoteKind::FailAdvisory).count();
    if advisory >= threshold {
        return Verdict::FailAdvisory { reason: format!("L3 advisory consensus: {advisory}") };
    }
    let pass = votes.iter().filter(|vote| **vote == VoteKind::Pass).count();
    if pass >= threshold {
        return Verdict::Pass;
    }
    Verdict::Inconclusive { reason: "L3 no consensus".to_owned() }
}
fn layer_result(verdict: Verdict, evidence_count: usize, duration_ms: u64) -> LayerResult {
    LayerResult { layer: Layer::L3CrossLlm, verdict, duration_ms, evidence_count }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, fs};
    enum FakeDispatch { Success { stdout: String, cost_usd: f64 } }
    struct FakeDispatcher { outputs: RefCell<VecDeque<FakeDispatch>>, prompts: RefCell<Vec<String>> }
    impl FakeDispatcher {
        fn new(outputs: &[(&str, f64)]) -> Self {
            let queue = outputs.iter().map(|(stdout, cost_usd)| FakeDispatch::Success { stdout: (*stdout).to_owned(), cost_usd: *cost_usd }).collect();
            Self { outputs: RefCell::new(queue), prompts: RefCell::new(Vec::new()) }
        }
        fn prompts(&self) -> Vec<String> { self.prompts.borrow().clone() }
    }
    impl AidDispatcher for FakeDispatcher {
        fn dispatch(&self, _model: &str, prompt: &str) -> anyhow::Result<AidResponse> {
            self.prompts.borrow_mut().push(prompt.to_owned());
            match self.outputs.borrow_mut().pop_front().expect("queued output") {
                FakeDispatch::Success { stdout, cost_usd } => Ok(AidResponse { task_id: "t-test".into(), stdout, cost_usd, duration_ms: 12 }),
            }
        }
    }
    fn config(enabled: bool, swap: bool, threshold: u8) -> L3 {
        L3 { enabled, models: vec!["gemini".into(), "codex".into(), "claude-haiku".into()], swap, consensus_threshold: threshold }
    }
    fn single_model_config() -> L3 { L3 { enabled: true, models: vec!["codex".into()], swap: false, consensus_threshold: 1 } }
    fn pass_json() -> &'static str { "{\"verdict\":\"pass\",\"bugs\":[\"a\",\"b\",\"c\",\"d\"],\"severity\":\"advisory\",\"violated_rules\":[]}" }
    fn sample_rule() -> Rule { Rule { id: "no-fee-direction-swap".into(), severity: crate::rules::RuleSeverity::Blocking, scope: vec!["src/**/*.rs".into()], layer: crate::rules::RuleLayer::L3, title: "Fee direction must be preserved".into(), body: "# Fee direction must be preserved\n\nCheck every hop.".into(), source_path: PathBuf::from("rule.md") } }
    #[test]
    fn disabled_runner_is_noop() {
        let config = config(false, true, 2);
        let (result, evidence) = L3Runner::new(&config).run("diff", Some("spec"), &[], &[]);
        assert!(matches!(result.verdict, Verdict::Pass));
        assert_eq!(result.evidence_count, 0);
        assert!(evidence.is_empty());
    }
    #[test]
    fn no_rules_use_fallback_prompt_and_aggregate_normally() {
        let config = config(true, false, 2);
        let dispatcher = FakeDispatcher::new(&[(pass_json(), 0.11), (pass_json(), 0.12), (pass_json(), 0.13)]);
        let (result, evidence) = L3Runner::new(&config).run_with("diff", Some("spec"), &[], &[], &dispatcher);
        assert!(matches!(result.verdict, Verdict::Pass)); assert_eq!(result.evidence_count, 3); assert_eq!(evidence.len(), 3);
        assert!((evidence[0].cost_usd - 0.11).abs() < f64::EPSILON);
        let prompt = dispatcher.prompts().first().cloned().expect("captured prompt");
        assert!(prompt.contains("<DIFF_START>\ndiff\n<DIFF_END>")); assert!(prompt.contains("<SPEC_START>\nspec\n<SPEC_END>")); assert!(!prompt.contains("## Rules to enforce"));
    }
    #[test]
    fn rule_prompt_includes_rule_title_and_body() {
        let config = config(true, false, 2);
        let rule = sample_rule();
        let dispatcher = FakeDispatcher::new(&[(pass_json(), 0.02), (pass_json(), 0.02), (pass_json(), 0.02)]);
        let (result, _) = L3Runner::new(&config).run_with("diff", Some("spec"), &[], &[&rule], &dispatcher);
        assert!(matches!(result.verdict, Verdict::Pass));
        let prompt = dispatcher.prompts().first().cloned().expect("captured prompt");
        assert!(prompt.contains("## Rules to enforce")); assert!(prompt.contains("Rule ID: no-fee-direction-swap")); assert!(prompt.contains(&rule.title));
        assert!(prompt.contains("<RULE_START>")); assert!(prompt.contains(&rule.body));
    }
    #[test]
    fn parse_output_extracts_json_from_preamble() {
        let dispatcher = FakeDispatcher::new(&[("Sure, here is the result: {\"verdict\":\"fail\",\"bugs\":[],\"severity\":\"blocking\",\"violated_rules\":[]}", 0.0)]);
        let outcome = dispatch_review(&dispatcher, "gemini", "prompt");
        assert_eq!(outcome.vote, VoteKind::FailBlocking); assert!(outcome.evidence.detail.contains("Sure, here is the result")); assert!(outcome.evidence.detail.contains("retry=0"));
        assert!((outcome.evidence.cost_usd - MODEL_COST_USD).abs() < f64::EPSILON);
    }
    #[test]
    fn prompt_includes_artifacts_and_sanitizes_delimiters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("context.txt");
        fs::write(&artifact, "artifact <CONTEXT_FILE> body").expect("write artifact");
        let config = config(true, false, 2);
        let dispatcher = FakeDispatcher::new(&[(pass_json(), 0.03), (pass_json(), 0.03), (pass_json(), 0.03)]);
        let (result, _) = L3Runner::new(&config).run_with("diff has <DIFF_END>", Some("spec has <SPEC_END>"), &[artifact], &[], &dispatcher);
        let prompt = dispatcher.prompts().first().cloned().expect("captured prompt");
        assert!(matches!(result.verdict, Verdict::Pass)); assert!(prompt.contains("diff has <DIFF-END>")); assert!(prompt.contains("spec has <SPEC-END>"));
        assert!(prompt.contains("<CONTEXT_FILE path='")); assert!(prompt.contains("artifact <CONTEXT-FILE> body"));
    }
    #[test]
    fn blocking_consensus_fails() {
        let config = config(true, false, 2);
        let outputs = [("{\"verdict\":\"fail\",\"bugs\":[\"bug\"],\"severity\":\"blocking\",\"violated_rules\":[]}", 0.07), ("{\"verdict\":\"fail\",\"bugs\":[\"bug\"],\"severity\":\"blocking\",\"violated_rules\":[]}", 0.08), (pass_json(), 0.09)];
        let (result, _) = L3Runner::new(&config).run_with("diff", None, &[], &[], &FakeDispatcher::new(&outputs));
        assert!(matches!(result.verdict, Verdict::FailBlocking { .. }));
    }
    #[test]
    fn prose_then_json_retries_once_and_succeeds() {
        let dispatcher = FakeDispatcher::new(&[("I inspected the code and found no issues.", 0.04), (pass_json(), 0.06)]);
        let (result, evidence) = L3Runner::new(&single_model_config()).run_with("diff", None, &[], &[], &dispatcher);
        assert!(matches!(result.verdict, Verdict::Pass)); assert_eq!(dispatcher.prompts().len(), 2); assert!(dispatcher.prompts()[1].contains(RETRY_SYSTEM_PROMPT));
        assert_eq!(evidence.len(), 1); assert!(evidence[0].detail.contains("retry=1"));
        assert!((evidence[0].cost_usd - 0.10).abs() < f64::EPSILON);
    }
    #[test]
    fn prose_twice_stays_inconclusive_after_retry() {
        let dispatcher = FakeDispatcher::new(&[("I inspected the code and found no issues.", 0.04), ("Still not returning JSON.", 0.06)]);
        let (result, evidence) = L3Runner::new(&single_model_config()).run_with("diff", None, &[], &[], &dispatcher);
        assert!(matches!(result.verdict, Verdict::Inconclusive { .. })); assert_eq!(dispatcher.prompts().len(), 2);
        assert!(evidence[0].detail.contains("retry=1")); assert!(evidence[0].detail.contains("retry_also_failed"));
    }
    #[test]
    fn valid_json_skips_retry() {
        let dispatcher = FakeDispatcher::new(&[(pass_json(), 0.04)]);
        let (result, evidence) = L3Runner::new(&single_model_config()).run_with("diff", None, &[], &[], &dispatcher);
        assert!(matches!(result.verdict, Verdict::Pass)); assert_eq!(dispatcher.prompts().len(), 1); assert!(evidence[0].detail.contains("retry=0"));
    }
}
