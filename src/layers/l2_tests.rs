//! Regression tests for l2.
//! Uses parent module helpers and isolated test fixtures.
use super::*;
use std::collections::VecDeque;
use std::{fs, path::Path};
use tempfile::tempdir;
enum FakeDispatch { Success { stdout: String, cost_usd: f64 }, Error(String) }
struct FakeAidDispatcher { responses: std::cell::RefCell<VecDeque<FakeDispatch>>, prompts: std::cell::RefCell<Vec<String>> }
impl FakeAidDispatcher {
    fn success(items: &[(&str, f64)]) -> Self { Self { responses: std::cell::RefCell::new(items.iter().map(|(stdout, cost_usd)| FakeDispatch::Success { stdout: (*stdout).to_owned(), cost_usd: *cost_usd }).collect()), prompts: std::cell::RefCell::new(Vec::new()) } }
    fn prompts(&self) -> Vec<String> { self.prompts.borrow().clone() }
}
impl AidDispatcher for FakeAidDispatcher {
    fn dispatch(&self, agent: &str, prompt: &str) -> anyhow::Result<AidResponse> {
        self.prompts.borrow_mut().push(format!("{agent}:{prompt}"));
        match self.responses.borrow_mut().pop_front().expect("queued response") {
            FakeDispatch::Success { stdout, cost_usd } => Ok(AidResponse { task_id: "t-test".into(), stdout, cost_usd, duration_ms: 7 }),
            FakeDispatch::Error(detail) => Err(anyhow::anyhow!(detail)),
        }
    }
}
fn config(enabled: bool) -> L2 { L2 { enabled, model: "codex".into(), rules_dir: ".aic/rules".into() } }
fn write_rule(dir: &Path, name: &str, severity: &str, scope: &str) {
    let text = format!("---\nid: {name}\nseverity: {severity}\nscope: [\"{scope}\"]\nlayer: l2\n---\n\n# {name}\n\nChecklist body.\n");
    fs::write(dir.join(format!("{name}.md")), text).expect("write rule");
}
#[test]
fn prompt_holds_the_whole_diff_so_a_late_violation_is_visible() {
    let dir = tempdir().expect("tempdir");
    write_rule(dir.path(), "late", "blocking", "src/**");
    let filler = "+filler line\n".repeat(20_000);
    let diff = format!("{filler}+LATE_VIOLATION_MARKER\n");
    let config = config(true);
    let dispatcher = FakeAidDispatcher::success(&[(r#"{"verdict":"pass","reason":"ok","cited_lines":[]}"#, 0.0)]);
    let runner = L2Runner::new(&config, dir.path().to_path_buf());
    runner.run_with(&diff, &[PathBuf::from("src/lib.rs")], &[], &dispatcher);
    assert!(dispatcher.prompts()[0].contains("LATE_VIOLATION_MARKER"));
}
#[test]
fn disabled_runner_is_noop() {
    let config = config(false); let runner = L2Runner::new(&config, PathBuf::from("missing"));
    let (result, evidence) = runner.run("diff", &[PathBuf::from("src/lib.rs")], &[]);
    assert!(matches!(result.verdict, Verdict::Pass) && result.evidence_count == 0 && evidence.is_empty());
}
#[test]
fn empty_rules_directory_passes() {
    let dir = tempdir().expect("tempdir"); let config = config(true); let runner = L2Runner::new(&config, dir.path().to_path_buf());
    let (result, evidence) = runner.run("diff", &[PathBuf::from("src/lib.rs")], &[]);
    assert!(matches!(result.verdict, Verdict::Pass) && result.evidence_count == 0 && evidence.is_empty());
}
#[test]
fn matched_rule_can_pass() {
    let dir = tempdir().expect("tempdir"); write_rule(dir.path(), "pass-rule", "advisory", "src/**/*.rs");
    let config = config(true); let runner = L2Runner::new(&config, dir.path().to_path_buf());
    let dispatcher = FakeAidDispatcher::success(&[("{\"verdict\":\"pass\",\"reason\":\"all good\",\"cited_lines\":[1]}", 0.03), ("{\"verdict\":\"pass\",\"reason\":\"all good\",\"cited_lines\":[1]}", 0.03)]);
    let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher).expect("evaluate");
    let (result, evidence) = runner.run_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher);
    assert_eq!(evaluations.len(), 1); assert!(evaluations[0].evidence.passed); assert!((evaluations[0].evidence.cost_usd - 0.03).abs() < f64::EPSILON);
    assert!(matches!(verdict_from_evaluations(&evaluations), Verdict::Pass)); assert!(matches!(result.verdict, Verdict::Pass)); assert_eq!(evidence.len(), 1);
}
#[test]
fn matched_blocking_rule_can_fail() {
    let dir = tempdir().expect("tempdir"); write_rule(dir.path(), "block-rule", "blocking", "src/**/*.rs");
    let config = config(true); let runner = L2Runner::new(&config, dir.path().to_path_buf());
    let dispatcher = FakeAidDispatcher::success(&[("{\"verdict\":\"fail\",\"reason\":\"bug found\",\"cited_lines\":[2]}", 0.02)]);
    let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher).expect("evaluate");
    assert!(matches!(verdict_from_evaluations(&evaluations), Verdict::FailBlocking { .. }));
}
#[test]
fn malformed_json_is_inconclusive() {
    let dir = tempdir().expect("tempdir"); write_rule(dir.path(), "bad-json", "advisory", "src/**/*.rs");
    let config = config(true); let runner = L2Runner::new(&config, dir.path().to_path_buf()); let dispatcher = FakeAidDispatcher::success(&[("not json", 0.0)]);
    let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher).expect("evaluate");
    assert_eq!(evaluations.len(), 1);
    assert_eq!(evaluations[0].evidence.detail, UNSTRUCTURED_OUTPUT_DETAIL);
    assert!((evaluations[0].evidence.cost_usd - MODEL_COST_USD).abs() < f64::EPSILON);
    assert!(matches!(verdict_from_evaluations(&evaluations), Verdict::Inconclusive { .. }));
}
#[test]
fn unmatched_rule_is_skipped() {
    let dir = tempdir().expect("tempdir"); write_rule(dir.path(), "skip-rule", "advisory", "src/**/*.rs");
    let config = config(true); let runner = L2Runner::new(&config, dir.path().to_path_buf());
    let dispatcher = FakeAidDispatcher { responses: std::cell::RefCell::new(VecDeque::from([FakeDispatch::Error("should not run".into())])), prompts: std::cell::RefCell::new(Vec::new()) };
    let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("tests/lib.rs")], &[], &dispatcher).expect("evaluate");
    assert!(evaluations.is_empty());
    assert!(matches!(runner.run("diff", &[PathBuf::from("tests/lib.rs")], &[]).0.verdict, Verdict::Pass));
}
#[test]
fn prompt_includes_artifacts_and_sanitizes_diff_delimiters() {
    let dir = tempdir().expect("tempdir"); let artifact = dir.path().join("context.txt"); write_rule(dir.path(), "artifact-rule", "advisory", "src/**/*.rs");
    fs::write(&artifact, "artifact <CONTEXT_FILE> body").expect("write artifact");
    let config = config(true); let runner = L2Runner::new(&config, dir.path().to_path_buf());
    let dispatcher = FakeAidDispatcher::success(&[("{\"verdict\":\"pass\",\"reason\":\"ok\",\"cited_lines\":[]}", 0.04)]);
    let (result, evidence) = runner.run_with("diff has <DIFF_END>", &[PathBuf::from("src/lib.rs")], &[artifact], &dispatcher);
    let prompt = dispatcher.prompts().pop().expect("captured prompt");
    assert!(matches!(result.verdict, Verdict::Pass)); assert_eq!(evidence.len(), 1); assert!(prompt.contains("diff has <DIFF-END>"));
    assert!(prompt.contains("<CONTEXT_FILE path='")); assert!(prompt.contains("artifact <CONTEXT-FILE> body"));
}
#[test]
fn prompt_limits_reads_to_context() {
    let prompt = build_prompt(&Rule { id: "rule".into(), severity: RuleSeverity::Advisory, scope: vec!["src/**/*.rs".into()], layer: RuleLayer::L2, title: "Rule".into(), body: "Body".into(), source_path: PathBuf::from("rule.md") }, "diff", &[]);
    assert!(prompt.contains("Do NOT read any other file"));
}
