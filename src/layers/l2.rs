// src/layers/l2.rs
// L2 checklist review. Exports: L2Runner; deps: config, rules, types, aid_dispatch, serde_json.

use crate::config::L2;
use crate::layers::aid_dispatch::{AidDispatcher, AidResponse, RealAidDispatcher};
use crate::layers::{render_artifact_context, sanitize_for_delimiters};
use crate::layers::json_extract::{repair_partial_json, SchemaHint};
use crate::layers::judge_output::judge_json;
use crate::rules::{load_rules_dir, Rule, RuleLayer, RuleSeverity};
use crate::types::{Evidence, Layer, LayerResult, SignalKind, Verdict};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Instant;

const PROMPT_DELIMITERS: &[&str] = &["<DIFF_START>", "<DIFF_END>", "<CONTEXT_FILE>", "</CONTEXT_FILE>"];
pub const MODEL_COST_USD: f64 = 0.01;
const UNSTRUCTURED_OUTPUT_DETAIL: &str = "model returned unstructured output (tool-call payload or prose)";

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
enum ParsedVerdict { Pass(String), Fail(String), Inconclusive(String) }
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
            .map(|rule| evaluate_rule(rule, &self.config.model, diff_text, artifacts, dispatcher))
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
fn evaluate_rule<D>(
    rule: &Rule, agent: &str, diff_text: &str, artifacts: &[PathBuf], dispatcher: &D,
) -> RuleEvaluation
where
    D: AidDispatcher + ?Sized,
{
    let prompt = build_prompt(rule, diff_text, artifacts);
    match dispatcher.dispatch(agent, &prompt) {
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
        Ok(parsed) => {
            let (passed, verdict, detail) = match parsed {
                ParsedVerdict::Pass(detail) => (true, Verdict::Pass, detail),
                ParsedVerdict::Fail(detail) => (false, rule_verdict(rule, false, &detail), detail),
                ParsedVerdict::Inconclusive(detail) => (false, Verdict::Inconclusive { reason: format!("{}: {detail}", rule.id) }, detail),
            };
            RuleEvaluation {
                verdict,
                evidence: Evidence {
                    layer: Layer::L2Checklist,
                    signal: SignalKind::Checklist,
                    passed,
                    detail,
                    cite: Some(rule.id.clone()),
                    cost_usd,
                },
            }
        }
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
    let diff_text = sanitize_for_delimiters(diff_text, PROMPT_DELIMITERS);
    format!("[SYSTEM]\nYou are a code review JSON API. You respond ONLY with a single JSON object.\nYou NEVER include reasoning, explanation, preamble, postamble, or markdown fences.\nCONSTRAINT: Respond based ONLY on the content between the delimiters shown below. Do NOT read any file other than this one, list directories, run greps, or make any other tool call. Do NOT ask for more context. If the diff is insufficient for a confident verdict, return verdict=\"pass\" with reason=\"insufficient_context\" — never fail on missing context alone. This is a static analysis task on the exact diff provided, not an investigation.\nYour entire response must be a valid JSON object parseable by serde_json.\n\nRequired schema (all fields mandatory):\n{{\"verdict\":\"pass\"|\"fail\",\"reason\":\"<one sentence>\",\"cited_lines\":[\"path:line\",\"...\"]}}\n\nExample response:\n{{\"verdict\":\"pass\",\"reason\":\"no violations\",\"cited_lines\":[]}}\n\n[TASK]\nApply this rule:\n\n<RULE_START>\nRule id: {}\nRule body: {}\n<RULE_END>\n\nTo this diff:\n\n<DIFF_START>\n{}\n<DIFF_END>{}\n\n[RESPONSE]\nRespond now with ONLY the JSON object. Your entire response must begin with {{ and end with }}.", rule.id, rule.body, diff_text, artifact_context)
}
fn parse_response(rule: &Rule, output: &str) -> Result<ParsedVerdict, String> {
    let json = match judge_json(output, SchemaHint::L2) {
        Some(json) => json,
        None => return Ok(ParsedVerdict::Inconclusive(UNSTRUCTURED_OUTPUT_DETAIL.to_owned())),
    };
    let response: ModelResponse = match serde_json::from_str(&json) {
        Ok(response) => response,
        Err(error) => match repair_partial_json(&json, SchemaHint::L2) {
            Some(repaired) => serde_json::from_str(&repaired).map_err(|repair_error| format!("invalid JSON: {error}; repair failed: {repair_error}"))?,
            None => return Ok(ParsedVerdict::Inconclusive(UNSTRUCTURED_OUTPUT_DETAIL.to_owned())),
        },
    };
    match response.verdict.as_str() {
        "pass" => Ok(ParsedVerdict::Pass(detail_or_default(rule, &response.reason, "passed"))),
        "fail" => Ok(ParsedVerdict::Fail(detail_or_default(rule, &response.reason, "failed"))),
        "inconclusive" => Ok(ParsedVerdict::Inconclusive(detail_or_default(rule, &response.reason, "inconclusive"))),
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
#[cfg(test)]
#[path = "l2_tests.rs"]
mod tests;
