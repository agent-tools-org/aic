// src/review/challenge.rs
// Reverse guard: a second judge must establish each high finding before it can block.
// Exports: Source, Outcome, Decision, Challenge, prompt, parse, settle; deps: review::impact, serde_json.
use super::impact::{Impact, RUBRIC};
use crate::layers::judge_output::strict_final_json;
use crate::layers::sanitize_for_delimiters;
use serde_json::Value;

const DELIMITERS: &[&str] = &["<DIFF_START>", "<DIFF_END>", "<CLAIMS_START>", "<CLAIMS_END>"];

/// Where the challenged findings come from, which decides what the challenger may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Numbered high findings from an aic judge that saw only the diff.
    Judge,
    /// A free-form audit report; its findings cite repository code beyond the diff.
    Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Upheld, Downgraded, Refuted, Inconclusive }

impl Outcome {
    fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "upheld" => Some(Self::Upheld),
            "downgraded" => Some(Self::Downgraded),
            "refuted" => Some(Self::Refuted),
            "inconclusive" => Some(Self::Inconclusive),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self { Self::Upheld => "upheld", Self::Downgraded => "downgraded", Self::Refuted => "refuted", Self::Inconclusive => "inconclusive" }
    }
}

/// One challenged finding after the downgrade-only rule is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision { pub finding: String, pub outcome: Outcome, pub impact: Impact, pub evidence: String }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge { pub high_findings: usize, pub decisions: Vec<Decision> }

impl Challenge {
    /// Findings still high after the challenge. Findings the challenger skipped stay high.
    pub fn upheld_high(&self) -> usize {
        let skipped = self.high_findings.saturating_sub(self.decisions.len());
        skipped + self.decisions.iter().filter(|decision| decision.impact == Impact::High).count()
    }
}

pub fn prompt(diff: &str, claims: &str, source: Source) -> String {
    let diff = sanitize_for_delimiters(diff, DELIMITERS);
    let claims = sanitize_for_delimiters(claims, DELIMITERS);
    let (what, reading) = match source {
        Source::Judge => ("The claims are numbered findings rated high; use the number as `finding`.",
            "Respond based ONLY on the content between the delimiters. Do NOT read any other file or make any tool call."),
        Source::Report => ("The claims are an audit report. Challenge every finding the report rates high, or that it uses to justify FIX or BLOCK; use the finding's title as `finding`.",
            "You may read repository files to trace a trigger. Do not modify anything."),
    };
    format!("[SYSTEM]\nYou are an audit challenger and a JSON API. You respond ONLY with a single JSON object.\n{reading}\n\n\
[TASK]\nEach claim says this change has a high-impact defect. For each one, establish whether it holds:\n\
1. Trace the trigger: name the concrete input or sequence of states that reaches the defect, with file:line.\n\
2. Name the actor who reaches it, and whether that actor already holds the capability the claim says they gain.\n\
3. Compare with the code before this change: is this version worse, or does it improve a problem without removing all of it?\n\
4. Rate the impact with the rubric below.\n\
Decide `upheld` when the trigger is traced, this version is worse than the one before it, and the rubric gives high; `downgraded` with the rubric's lower impact otherwise; \
`refuted` when the described behaviour does not occur; `inconclusive` when you could not read what you needed. \
Every downgraded or refuted decision must quote the evidence that decides it.\n\n{RUBRIC}\n\n{what}\n\n\
Required schema:\n{{\"high_findings\":<number of claims you challenged>,\"decisions\":[{{\"finding\":\"<id>\",\"decision\":\"upheld\"|\"downgraded\"|\"refuted\"|\"inconclusive\",\"impact\":\"high\"|\"medium\"|\"low\",\"evidence\":\"<file:line and reason>\"}}]}}\n\n\
## Claims\n<CLAIMS_START>\n{claims}\n<CLAIMS_END>\n\n## Diff\n<DIFF_START>\n{diff}\n<DIFF_END>\n\n\
[RESPONSE]\nRespond now with ONLY the JSON object.")
}

/// Parses a challenger answer. `None` when it is not one complete JSON object of this schema.
pub fn parse(output: &str) -> Option<Challenge> {
    let value = strict_final_json(output)?;
    let parsed = value.get("decisions")?.as_array()?.iter().map(parse_decision).collect::<Option<Vec<_>>>()?;
    let decisions = keep_highest_per_finding(parsed);
    let reported = value.get("high_findings").and_then(Value::as_u64).map_or(0, |count| count as usize);
    Some(Challenge { high_findings: reported.max(decisions.len()), decisions })
}

/// Repeated decisions for one finding collapse to the highest impact any of them leaves.
fn keep_highest_per_finding(parsed: Vec<Decision>) -> Vec<Decision> {
    let mut decisions: Vec<Decision> = Vec::new();
    for decision in parsed {
        match decisions.iter_mut().find(|kept| kept.finding == decision.finding) {
            Some(kept) if decision.impact > kept.impact => *kept = decision,
            Some(_) => {}
            None => decisions.push(decision),
        }
    }
    decisions
}

/// Downgrade-only: a challenger can lower a high finding, never raise one, and must cite file:line.
/// A decision without a finding id makes the whole answer invalid.
fn parse_decision(value: &Value) -> Option<Decision> {
    let finding = value.get("finding").map(value_text)?.trim().trim_start_matches('#').to_owned();
    if finding.is_empty() {
        return None;
    }
    let outcome = Outcome::parse(value.get("decision")?.as_str()?)?;
    let evidence = value.get("evidence").and_then(Value::as_str).unwrap_or("").trim().to_owned();
    let stated = value.get("impact").and_then(Value::as_str).and_then(Impact::parse);
    let lowered = match outcome {
        Outcome::Refuted => Some(Impact::Low),
        Outcome::Downgraded => stated.filter(|impact| *impact < Impact::High),
        Outcome::Upheld | Outcome::Inconclusive => None,
    };
    let (outcome, impact) = match lowered {
        Some(impact) if cites_location(&evidence) => (outcome, impact),
        _ if outcome == Outcome::Inconclusive => (outcome, Impact::High),
        _ => (Outcome::Upheld, Impact::High),
    };
    Some(Decision { finding, outcome, impact, evidence })
}

/// True when the text names a `file.ext:line` location.
fn cites_location(text: &str) -> bool {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, '(' | ')' | ',' | ';' | '`' | '[' | ']'))
        .any(|word| word.split_once(':').is_some_and(|(path, line)| {
            path.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
                && line.starts_with(|ch: char| ch.is_ascii_digit())
        }))
}

fn value_text(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
}

/// The challenged impact of numbered judge finding `number` (1-based); high when not decided.
pub fn settle(challenge: &Challenge, number: usize) -> Impact {
    let id = number.to_string();
    challenge.decisions.iter().find(|decision| decision.finding == id).map_or(Impact::High, |decision| decision.impact)
}

#[cfg(test)]
#[path = "challenge_tests.rs"]
mod tests;
