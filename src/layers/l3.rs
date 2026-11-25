// src/layers/l3.rs
// L3 cross-LLM voter with impact-rated findings; high findings face a challenger before they block.
// Exports: L3Runner; deps: config, review, types, serde_json.
use crate::config::L3;
use crate::layers::aid_dispatch::{AidDispatcher, AidResponse, RealAidDispatcher};
use crate::layers::{render_artifact_context, sanitize_for_delimiters};
use crate::layers::json_extract::{repair_partial_json, SchemaHint};
use crate::layers::judge_output::judge_json_marked;
use crate::review::challenge::{self, Source};
use crate::review::impact::{max_impact, parse_bugs, rank_bugs, Bug, Impact, RUBRIC};
use crate::rules::{Rule, RuleSeverity};
use crate::types::{Evidence, Layer, LayerResult, SignalKind, Verdict};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;
const PROMPT_DELIMITERS: &[&str] = &["<DIFF_START>", "<DIFF_END>", "<CONTEXT_FILE>", "</CONTEXT_FILE>", "<SPEC_START>", "<SPEC_END>"];
pub const MODEL_COST_USD: f64 = 0.05;
const MAX_BUGS: usize = 3;
const SCHEMA: &str = "{\"verdict\":\"pass\"|\"fail\"|\"inconclusive\",\"bugs\":[{\"impact\":\"high\"|\"medium\"|\"low\",\"bug\":\"<one-line description>\",\"trigger\":\"<concrete input or sequence of states>\"}],\"violated_rules\":[\"id\",\"...\"]}";
const RETRY_SYSTEM_PROMPT: &str = "CRITICAL: Your previous response contained no JSON object. You MUST respond with ONLY a JSON object matching this schema: {verdict, bugs: [{impact, bug, trigger}], violated_rules}. Start with { and end with }. No reasoning or preamble. Read only this context file and make no other tool call. This is a structured-output API, not an interactive chat.";
const UNSTRUCTURED_OUTPUT_DETAIL: &str = "model returned unstructured output (tool-call payload or prose)";
const UNRATED_BUG_DETAIL: &str = "model returned a bug without a valid impact and description";
pub struct L3Runner<'cfg> { pub config: &'cfg L3 }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoteKind { Pass, FailBlocking, FailAdvisory, Inconclusive }
#[derive(Debug, Clone)]
/// `repaired` marks an answer rebuilt from partial JSON; it can fail a change but never pass it.
struct Review { verdict: String, bugs: Vec<Bug>, violated_rules: Vec<String>, repaired: bool }
#[derive(Debug, Clone)]
struct CallOutcome { evidence: Evidence, vote: VoteKind, review: Option<Review> }
#[derive(Debug, Deserialize)]
struct ReviewResponse { verdict: String, #[serde(default)] bugs: Vec<Value>, #[serde(default)] violated_rules: Vec<String> }
enum ParseStatus { Parsed(Review), #[allow(dead_code)] Retry(String), Inconclusive(String) }
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
        let mut evaluation = evaluate_models(self.config, diff_text, spec, artifacts, rules, dispatcher);
        let mut verdict = aggregate_votes(self.config.consensus_threshold, &evaluation.votes);
        if matches!(verdict, Verdict::FailBlocking { .. }) {
            challenge_blocking(self.config, diff_text, rules, &mut evaluation, dispatcher);
            verdict = aggregate_votes(self.config.consensus_threshold, &evaluation.votes);
        }
        let evidence = evaluation.evidence;
        (layer_result(verdict, evidence.len(), started_at.elapsed().as_millis() as u64), evidence)
    }
}
struct Evaluation { evidence: Vec<Evidence>, votes: Vec<VoteKind>, reviews: Vec<Option<Review>> }
fn evaluate_models<D>(config: &L3, diff_text: &str, spec: Option<&str>, artifacts: &[PathBuf], rules: &[&Rule], dispatcher: &D) -> Evaluation where D: AidDispatcher + ?Sized {
    let mut evaluation = Evaluation { evidence: Vec::new(), votes: Vec::new(), reviews: Vec::new() };
    let prompt = build_prompt(diff_text, spec, artifacts, rules, false);
    let swapped_prompt = config.swap.then(|| build_prompt(diff_text, spec, artifacts, rules, true));
    for model in &config.models {
        let normal = dispatch_review(dispatcher, model, &prompt, rules);
        evaluation.evidence.push(normal.evidence);
        let mut swapped_review = None;
        let vote = if config.swap {
            let swapped = dispatch_review(dispatcher, model, swapped_prompt.as_deref().unwrap_or(&prompt), rules);
            evaluation.evidence.push(swapped.evidence);
            swapped_review = swapped.review;
            if normal.vote == swapped.vote { normal.vote } else { VoteKind::Inconclusive }
        } else {
            normal.vote
        };
        evaluation.votes.push(vote);
        evaluation.reviews.push(merge_reviews(normal.review, swapped_review));
    }
    evaluation
}
/// Both swap positions' findings face the challenge, so neither side's high finding is dropped.
fn merge_reviews(normal: Option<Review>, swapped: Option<Review>) -> Option<Review> {
    let (mut merged, other) = match (normal, swapped) {
        (Some(normal), Some(swapped)) => (normal, swapped),
        (review, None) | (None, review) => return review,
    };
    for bug in other.bugs {
        if !merged.bugs.iter().any(|kept| kept.bug == bug.bug && kept.trigger == bug.trigger) { merged.bugs.push(bug); }
    }
    merged.violated_rules.extend(other.violated_rules);
    merged.violated_rules.sort();
    merged.violated_rules.dedup();
    merged.repaired |= other.repaired;
    merged.bugs = rank_bugs(merged.bugs, MAX_BUGS);
    Some(merged)
}
/// Each blocking vote's high findings go to the next configured model, which may only lower them.
fn challenge_blocking<D>(config: &L3, diff_text: &str, rules: &[&Rule], evaluation: &mut Evaluation, dispatcher: &D) where D: AidDispatcher + ?Sized {
    for index in 0..evaluation.votes.len() {
        if evaluation.votes[index] != VoteKind::FailBlocking { continue; }
        let Some(review) = evaluation.reviews[index].as_mut() else { continue };
        let high: Vec<usize> = (0..review.bugs.len()).filter(|at| review.bugs[*at].impact == Impact::High).collect();
        if high.is_empty() { continue; }
        let challenger = &config.models[(index + 1) % config.models.len()];
        let claims = high.iter().enumerate().map(|(number, at)| format!("{}. {} (trigger: {})", number + 1, review.bugs[*at].bug, review.bugs[*at].trigger)).collect::<Vec<_>>().join("\n");
        let (parsed, detail, cost_usd) = match dispatcher.dispatch(challenger, &challenge::prompt(diff_text, &claims, Source::Judge)) {
            Ok(response) => { let output = response.stdout.trim().to_owned(); (challenge::parse(&output), output, response_cost(&response)) }
            Err(error) => (None, format!("dispatch error: {error}"), 0.0),
        };
        let upheld = match &parsed {
            Some(result) => {
                for (number, at) in high.iter().enumerate() { review.bugs[*at].impact = challenge::settle(result, number + 1); }
                evaluation.votes[index] = classify_vote(review, rules);
                format!("{} of {} high findings upheld", result.upheld_high(), high.len())
            }
            None => "challenge unparsed; findings stand".to_owned(),
        };
        let cite = config.models[index].as_str();
        evaluation.evidence.push(Evidence { layer: Layer::L3CrossLlm, signal: SignalKind::LlmVote, passed: evaluation.votes[index] != VoteKind::FailBlocking, detail: format!("challenge of {cite}: {upheld}; response={detail}"), cite: Some(challenger.clone()), cost_usd });
    }
}
fn dispatch_review<D>(dispatcher: &D, model: &str, prompt: &str, rules: &[&Rule]) -> CallOutcome where D: AidDispatcher + ?Sized {
    match dispatcher.dispatch(model, prompt) {
        Ok(response) => {
            let output = response.stdout.trim().to_owned();
            let cost_usd = response_cost(&response);
            match parse_output(&output) {
                ParseStatus::Parsed(review) => parsed_outcome(model, &output, review, rules, cost_usd, 0),
                ParseStatus::Inconclusive(detail) => inconclusive_outcome(model, detail, cost_usd),
                ParseStatus::Retry(_) => match dispatcher.dispatch(model, &retry_prompt(prompt)) {
                    Ok(retry_response) => {
                        let retry_output = retry_response.stdout.trim().to_owned();
                        let retry_cost_usd = cost_usd + response_cost(&retry_response);
                        match parse_output(&retry_output) {
                            ParseStatus::Parsed(review) => parsed_outcome(model, &retry_output, review, rules, retry_cost_usd, 1),
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
    let Some((json, mut repaired)) = judge_json_marked(output, SchemaHint::L3) else { return ParseStatus::Inconclusive(UNSTRUCTURED_OUTPUT_DETAIL.to_owned()) };
    let response = match serde_json::from_str::<ReviewResponse>(&json) {
        Ok(response) => response,
        Err(_) => match repair_partial_json(&json, SchemaHint::L3).map(|repaired| serde_json::from_str::<ReviewResponse>(&repaired)) {
            Some(Ok(response)) => { repaired = true; response }
            Some(Err(error)) => return ParseStatus::Inconclusive(format!("invalid JSON: {error}")),
            None => return ParseStatus::Inconclusive(UNSTRUCTURED_OUTPUT_DETAIL.to_owned()),
        },
    };
    let Some(bugs) = parse_bugs(&response.bugs) else { return ParseStatus::Inconclusive(UNRATED_BUG_DETAIL.to_owned()) };
    let mut violated_rules: Vec<String> = response.violated_rules.into_iter().filter(|id| !id.trim().is_empty()).collect();
    violated_rules.sort();
    violated_rules.dedup();
    ParseStatus::Parsed(Review { verdict: response.verdict, bugs: rank_bugs(bugs, MAX_BUGS), violated_rules, repaired })
}
/// The vote follows the rated findings and the declared severity of violated rules, not the judge's word.
fn classify_vote(review: &Review, rules: &[&Rule]) -> VoteKind {
    if review.verdict != "pass" && review.verdict != "fail" { return VoteKind::Inconclusive; }
    let rule_impact = rules.iter().filter(|rule| review.violated_rules.contains(&rule.id))
        .map(|rule| match rule.severity { RuleSeverity::Blocking => Impact::High, RuleSeverity::Advisory => Impact::Medium }).max();
    let vote = match max_impact(&review.bugs).max(rule_impact) {
        Some(Impact::High) => VoteKind::FailBlocking,
        Some(Impact::Medium) => VoteKind::FailAdvisory,
        Some(Impact::Low) => VoteKind::Pass,
        None if review.verdict == "fail" => VoteKind::Inconclusive,
        None => VoteKind::Pass,
    };
    if review.repaired && vote == VoteKind::Pass { VoteKind::Inconclusive } else { vote }
}
fn inconclusive_outcome(model: &str, detail: String, cost_usd: f64) -> CallOutcome {
    CallOutcome { vote: VoteKind::Inconclusive, review: None, evidence: Evidence { layer: Layer::L3CrossLlm, signal: SignalKind::LlmVote, passed: false, detail, cite: Some(model.to_owned()), cost_usd } }
}
fn parsed_outcome(model: &str, output: &str, review: Review, rules: &[&Rule], cost_usd: f64, retry_count: u8) -> CallOutcome {
    let violated_rules = format!("[{}]", review.violated_rules.join(", "));
    let evidence = Evidence { layer: Layer::L3CrossLlm, signal: SignalKind::LlmVote, passed: review.verdict == "pass", detail: format!("violated_rules={violated_rules}; retry={retry_count}; response={output}"), cite: Some(model.to_owned()), cost_usd };
    CallOutcome { vote: classify_vote(&review, rules), evidence, review: Some(review) }
}
fn response_cost(response: &AidResponse) -> f64 { if response.cost_usd == 0.0 { MODEL_COST_USD } else { response.cost_usd } }
fn retry_prompt(prompt: &str) -> String { format!("[SYSTEM]\n{RETRY_SYSTEM_PROMPT}\n\n{prompt}") }
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
        "[SYSTEM]\nYou are a code review JSON API. You respond ONLY with a single JSON object.\nYou NEVER include reasoning, explanation, preamble, postamble, or markdown fences.\nCONSTRAINT: Respond based ONLY on the content between the delimiters shown below. Read every line of this file; if a tool shortens its output, keep reading the rest until you have seen all of it. If you could not see every line, return verdict=\"inconclusive\". Do NOT read any other file, list directories, run greps, or make any other tool call. Do NOT ask for more context. If you saw every line and the diff is still insufficient for a confident verdict, return verdict=\"pass\" with reason=\"insufficient_context\" — never fail on missing context alone. This is a static analysis task on the exact diff provided, not an investigation.\nYour entire response must be a valid JSON object parseable by serde_json.\n\nRequired schema (all fields mandatory):\n{SCHEMA}\n\n{RUBRIC}\nOnly high and medium findings fail the change; list low findings too.\n\nExample response:\n{{\"verdict\":\"pass\",\"bugs\":[],\"violated_rules\":[]}}\n\n[TASK]\n{task}{rules_section}\n\n{ordered_sections}\n\n[RESPONSE]\nRespond now with ONLY the JSON object. Your entire response must begin with {{ and end with }}.",
        rules_section = rules_text.map(|text| format!("\n\n## Rules to enforce\n{text}")).unwrap_or_default(),
    )
}
fn aggregate_votes(threshold: u8, votes: &[VoteKind]) -> Verdict {
    let threshold = usize::from(threshold);
    let count = |kind: VoteKind| votes.iter().filter(|vote| **vote == kind).count();
    let blocking = count(VoteKind::FailBlocking);
    if blocking >= threshold { return Verdict::FailBlocking { reason: format!("L3 blocking consensus: {blocking}") }; }
    let advisory = count(VoteKind::FailAdvisory);
    if advisory >= threshold { return Verdict::FailAdvisory { reason: format!("L3 advisory consensus: {advisory}") }; }
    if count(VoteKind::Pass) >= threshold { return Verdict::Pass; }
    Verdict::Inconclusive { reason: "L3 no consensus".to_owned() }
}
fn layer_result(verdict: Verdict, evidence_count: usize, duration_ms: u64) -> LayerResult {
    LayerResult { layer: Layer::L3CrossLlm, verdict, duration_ms, evidence_count }
}
#[cfg(test)]
#[path = "l3_tests.rs"]
mod tests;
