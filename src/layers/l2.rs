// src/layers/l2.rs
// L2 checklist review. Exports: L2Runner; deps: config, rules, types, aid_dispatch, serde_json.

use crate::config::L2;
use crate::layers::aid_dispatch::{AidDispatcher, AidResponse, RealAidDispatcher};
use crate::layers::{render_artifact_context, sanitize_for_delimiters};
use crate::layers::json_extract::{extract_json_object, repair_partial_json, SchemaHint};
use crate::rules::{load_rules_dir, Rule, RuleLayer, RuleSeverity};
use crate::types::{Evidence, Layer, LayerResult, SignalKind, Verdict};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Instant;

const MAX_DIFF_BYTES: usize = 8 * 1024;
const PROMPT_DELIMITERS: &[&str] = &["<DIFF_START>", "<DIFF_END>", "<CONTEXT_FILE>", "</CONTEXT_FILE>"];
pub const MODEL_COST_USD: f64 = 0.01;

pub struct L2Runner<'cfg> { pub config: &'cfg L2, pub rules_dir: PathBuf }
#[derive(Debug)]
struct RuleEvaluation {
    #[cfg_attr(not(test), allow(dead_code))]
    evidence: Evidence,
    verdict: Verdict,
}
#[derive(Debug, Deserialize)]
struct ModelResponse {
    verdict: String,
    #[serde(default)]
    reason: String,
    #[serde(default, rename = "cited_lines")]
    _cited_lines: Vec<serde_json::Value>,
}
impl<'cfg> L2Runner<'cfg> {
    pub fn new(config: &'cfg L2, rules_dir: PathBuf) -> Self { Self { config, rules_dir } }
    pub fn run(&self, diff_text: &str, touched_files: &[PathBuf], artifacts: &[PathBuf]) -> (LayerResult, Vec<Evidence>) { self.run_with(diff_text, touched_files, artifacts, &RealAidDispatcher) }

    pub(crate) fn run_with<D>(&self, diff_text: &str, touched_files: &[PathBuf], artifacts: &[PathBuf], dispatcher: &D) -> (LayerResult, Vec<Evidence>)
    where
        D: AidDispatcher + ?Sized,
    {
        let started_at = Instant::now();
        let (verdict, evidence) = match self.evaluate_rules_with(diff_text, touched_files, artifacts, dispatcher) {
            Ok(evaluations) => evaluations_to_outcome(evaluations),
            Err(error) => (Verdict::Inconclusive { reason: error }, Vec::new()),
        };
        let result = LayerResult {
            layer: Layer::L2Checklist,
            verdict,
            duration_ms: started_at.elapsed().as_millis() as u64,
            evidence_count: evidence.len(),
        };
        (result, evidence)
    }
    fn evaluate_rules_with<D>(&self, diff_text: &str, touched_files: &[PathBuf], artifacts: &[PathBuf], dispatcher: &D) -> Result<Vec<RuleEvaluation>, String>
    where
        D: AidDispatcher + ?Sized,
    {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        let rules = load_rules_dir(&self.rules_dir).map_err(|error| format!("L2 rule load error: {error}"))?;
        Ok(matched_rules(&rules, touched_files)
            .into_iter()
            .map(|rule| evaluate_rule(rule, diff_text, artifacts, dispatcher))
            .collect())
    }
}
fn matched_rules<'a>(rules: &'a [Rule], touched_files: &[PathBuf]) -> Vec<&'a Rule> {
    rules.iter().filter(|rule| rule.layer == RuleLayer::L2).filter(|rule| touched_files.iter().any(|path| rule.matches(path))).collect()
}
fn evaluations_to_outcome(evaluations: Vec<RuleEvaluation>) -> (Verdict, Vec<Evidence>) {
    let verdict = verdict_from_evaluations(&evaluations);
    let evidence = evaluations.into_iter().map(|evaluation| evaluation.evidence).collect();
    (verdict, evidence)
}
fn evaluate_rule<D>(rule: &Rule, diff_text: &str, artifacts: &[PathBuf], dispatcher: &D) -> RuleEvaluation
where
    D: AidDispatcher + ?Sized,
{
    let prompt = build_prompt(rule, diff_text, artifacts);
    match dispatcher.dispatch("gemini", &prompt) {
        Ok(response) => parse_rule_evaluation(rule, response),
        Err(error) => RuleEvaluation {
            verdict: Verdict::Inconclusive { reason: format!("{}: {error}", rule.id) },
            evidence: Evidence {
                layer: Layer::L2Checklist,
                signal: SignalKind::Checklist,
                passed: false,
                detail: format!("L2 dispatch error: {error}"),
                cite: Some(rule.id.clone()),
                cost_usd: 0.0,
            },
        },
    }
}
fn parse_rule_evaluation(rule: &Rule, response: AidResponse) -> RuleEvaluation {
    let cost_usd = if response.cost_usd == 0.0 { MODEL_COST_USD } else { response.cost_usd };
    match parse_response(rule, &response.stdout) {
        Ok((passed, detail)) => RuleEvaluation {
            verdict: rule_verdict(rule, passed, &detail),
            evidence: Evidence {
                layer: Layer::L2Checklist,
                signal: SignalKind::Checklist,
                passed,
                detail,
                cite: Some(rule.id.clone()),
                cost_usd,
            },
        },
        Err(error) => RuleEvaluation {
            verdict: Verdict::Inconclusive { reason: format!("{}: {error}", rule.id) },
            evidence: Evidence {
                layer: Layer::L2Checklist,
                signal: SignalKind::Checklist,
                passed: false,
                detail: format!("L2 dispatch error: {error}"),
                cite: Some(rule.id.clone()),
                cost_usd,
            },
        },
    }
}
fn build_prompt(rule: &Rule, diff_text: &str, artifacts: &[PathBuf]) -> String {
    let artifact_context = render_artifact_context(artifacts, PROMPT_DELIMITERS);
    let diff_text = sanitize_for_delimiters(truncate_bytes(diff_text, MAX_DIFF_BYTES), PROMPT_DELIMITERS);
    format!("[SYSTEM]\nYou are a code review JSON API. You respond ONLY with a single JSON object.\nYou NEVER include reasoning, explanation, preamble, postamble, or markdown fences.\nYour entire response must be a valid JSON object parseable by serde_json.\n\nRequired schema (all fields mandatory):\n{{\"verdict\":\"pass\"|\"fail\",\"reason\":\"<one sentence>\",\"cited_lines\":[\"path:line\",\"...\"]}}\n\nExample response:\n{{\"verdict\":\"pass\",\"reason\":\"no violations\",\"cited_lines\":[]}}\n\n[TASK]\nApply this rule:\n\n<RULE_START>\nRule id: {}\nRule body: {}\n<RULE_END>\n\nTo this diff:\n\n<DIFF_START>\n{}\n<DIFF_END>{}\n\n[RESPONSE]\nRespond now with ONLY the JSON object. Your entire response must begin with {{ and end with }}.", rule.id, rule.body, diff_text, artifact_context)
}
fn parse_response(rule: &Rule, output: &str) -> Result<(bool, String), String> {
    let json = extract_json_object(output).map(str::to_owned).or_else(|| repair_partial_json(output, SchemaHint::L2)).ok_or_else(|| format!("no JSON object found in model output: {}", output.trim()))?;
    let response: ModelResponse = match serde_json::from_str(&json) {
        Ok(response) => response,
        Err(error) => serde_json::from_str(&repair_partial_json(&json, SchemaHint::L2).ok_or_else(|| format!("invalid JSON: {error}"))?).map_err(|repair_error| format!("invalid JSON: {error}; repair failed: {repair_error}"))?,
    };
    match response.verdict.as_str() {
        "pass" => Ok((true, detail_or_default(rule, &response.reason, "passed"))),
        "fail" => Ok((false, detail_or_default(rule, &response.reason, "failed"))),
        other => Err(format!("invalid verdict: {other}")),
    }
}
fn detail_or_default(rule: &Rule, reason: &str, suffix: &str) -> String {
    if reason.trim().is_empty() {
        format!("{} {suffix}", rule.id)
    } else {
        reason.to_owned()
    }
}
fn rule_verdict(rule: &Rule, passed: bool, detail: &str) -> Verdict {
    if passed {
        return Verdict::Pass;
    }
    match rule.severity {
        RuleSeverity::Blocking => Verdict::FailBlocking { reason: format!("{}: {detail}", rule.id) },
        RuleSeverity::Advisory => Verdict::FailAdvisory { reason: format!("{}: {detail}", rule.id) },
    }
}
fn verdict_from_evaluations(evaluations: &[RuleEvaluation]) -> Verdict {
    if let Some(reason) = joined_reasons(evaluations, |item| match &item.verdict {
        Verdict::FailBlocking { reason } => Some(reason.as_str()),
        _ => None,
    }) {
        return Verdict::FailBlocking { reason };
    }
    if let Some(reason) = joined_reasons(evaluations, |item| match &item.verdict {
        Verdict::FailAdvisory { reason } => Some(reason.as_str()),
        _ => None,
    }) {
        return Verdict::FailAdvisory { reason };
    }
    if let Some(reason) = joined_reasons(evaluations, |item| match &item.verdict {
        Verdict::Inconclusive { reason } => Some(reason.as_str()),
        _ => None,
    }) {
        return Verdict::Inconclusive { reason };
    }
    Verdict::Pass
}
fn joined_reasons<F>(evaluations: &[RuleEvaluation], select: F) -> Option<String>
where
    F: Fn(&RuleEvaluation) -> Option<&str>,
{
    let reasons: Vec<&str> = evaluations.iter().filter_map(select).collect();
    (!reasons.is_empty()).then(|| reasons.join("; "))
}
fn truncate_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes { return text; }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
#[cfg(test)]
mod tests {
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
    fn config(enabled: bool) -> L2 { L2 { enabled, model: "gemini-flash".into(), rules_dir: ".aic/rules".into() } }
    fn write_rule(dir: &Path, name: &str, severity: &str, scope: &str) {
        let text = format!("---\nid: {name}\nseverity: {severity}\nscope: [\"{scope}\"]\nlayer: l2\n---\n\n# {name}\n\nChecklist body.\n");
        fs::write(dir.join(format!("{name}.md")), text).expect("write rule");
    }
    #[test]
    fn disabled_runner_is_noop() {
        let config = config(false);
        let runner = L2Runner::new(&config, PathBuf::from("missing"));
        let (result, evidence) = runner.run("diff", &[PathBuf::from("src/lib.rs")], &[]);
        assert!(matches!(result.verdict, Verdict::Pass) && result.evidence_count == 0 && evidence.is_empty());
    }
    #[test]
    fn empty_rules_directory_passes() {
        let dir = tempdir().expect("tempdir");
        let config = config(true);
        let runner = L2Runner::new(&config, dir.path().to_path_buf());
        let (result, evidence) = runner.run("diff", &[PathBuf::from("src/lib.rs")], &[]);
        assert!(matches!(result.verdict, Verdict::Pass) && result.evidence_count == 0 && evidence.is_empty());
    }
    #[test]
    fn matched_rule_can_pass() {
        let dir = tempdir().expect("tempdir");
        write_rule(dir.path(), "pass-rule", "advisory", "src/**/*.rs");
        let config = config(true);
        let runner = L2Runner::new(&config, dir.path().to_path_buf());
        let dispatcher = FakeAidDispatcher::success(&[("{\"verdict\":\"pass\",\"reason\":\"all good\",\"cited_lines\":[1]}", 0.03), ("{\"verdict\":\"pass\",\"reason\":\"all good\",\"cited_lines\":[1]}", 0.03)]);
        let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher).expect("evaluate");
        let (result, evidence) = runner.run_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher);
        assert_eq!(evaluations.len(), 1);
        assert!(evaluations[0].evidence.passed);
        assert!((evaluations[0].evidence.cost_usd - 0.03).abs() < f64::EPSILON);
        assert!(matches!(verdict_from_evaluations(&evaluations), Verdict::Pass));
        assert!(matches!(result.verdict, Verdict::Pass));
        assert_eq!(evidence.len(), 1);
    }
    #[test]
    fn matched_blocking_rule_can_fail() {
        let dir = tempdir().expect("tempdir");
        write_rule(dir.path(), "block-rule", "blocking", "src/**/*.rs");
        let config = config(true);
        let runner = L2Runner::new(&config, dir.path().to_path_buf());
        let dispatcher = FakeAidDispatcher::success(&[("{\"verdict\":\"fail\",\"reason\":\"bug found\",\"cited_lines\":[2]}", 0.02)]);
        let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher).expect("evaluate");
        assert!(matches!(verdict_from_evaluations(&evaluations), Verdict::FailBlocking { .. }));
    }
    #[test]
    fn malformed_json_is_inconclusive() {
        let dir = tempdir().expect("tempdir");
        write_rule(dir.path(), "bad-json", "advisory", "src/**/*.rs");
        let config = config(true);
        let runner = L2Runner::new(&config, dir.path().to_path_buf());
        let dispatcher = FakeAidDispatcher::success(&[("not json", 0.0)]);
        let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("src/lib.rs")], &[], &dispatcher).expect("evaluate");
        assert_eq!(evaluations.len(), 1);
        assert!(evaluations[0].evidence.detail.starts_with("L2 dispatch error:"));
        assert!((evaluations[0].evidence.cost_usd - MODEL_COST_USD).abs() < f64::EPSILON);
        assert!(matches!(verdict_from_evaluations(&evaluations), Verdict::Inconclusive { .. }));
    }
    #[test]
    fn unmatched_rule_is_skipped() {
        let dir = tempdir().expect("tempdir");
        write_rule(dir.path(), "skip-rule", "advisory", "src/**/*.rs");
        let config = config(true);
        let runner = L2Runner::new(&config, dir.path().to_path_buf());
        let dispatcher = FakeAidDispatcher { responses: std::cell::RefCell::new(VecDeque::from([FakeDispatch::Error("should not run".into())])), prompts: std::cell::RefCell::new(Vec::new()) };
        let evaluations = runner.evaluate_rules_with("diff", &[PathBuf::from("tests/lib.rs")], &[], &dispatcher).expect("evaluate");
        assert!(evaluations.is_empty());
        assert!(matches!(runner.run("diff", &[PathBuf::from("tests/lib.rs")], &[]).0.verdict, Verdict::Pass));
    }
    #[test]
    fn prompt_includes_artifacts_and_sanitizes_diff_delimiters() {
        let dir = tempdir().expect("tempdir");
        let artifact = dir.path().join("context.txt");
        write_rule(dir.path(), "artifact-rule", "advisory", "src/**/*.rs");
        fs::write(&artifact, "artifact <CONTEXT_FILE> body").expect("write artifact");
        let config = config(true);
        let runner = L2Runner::new(&config, dir.path().to_path_buf());
        let dispatcher = FakeAidDispatcher::success(&[("{\"verdict\":\"pass\",\"reason\":\"ok\",\"cited_lines\":[]}", 0.04)]);
        let (result, evidence) = runner.run_with("diff has <DIFF_END>", &[PathBuf::from("src/lib.rs")], &[artifact], &dispatcher);
        let prompt = dispatcher.prompts().pop().expect("captured prompt");
        assert!(matches!(result.verdict, Verdict::Pass));
        assert_eq!(evidence.len(), 1);
        assert!(prompt.contains("diff has <DIFF-END>"));
        assert!(prompt.contains("<CONTEXT_FILE path='"));
        assert!(prompt.contains("artifact <CONTEXT-FILE> body"));
    }
}
