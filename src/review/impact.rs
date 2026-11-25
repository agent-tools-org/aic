// src/review/impact.rs
// Finding impact levels and the rubric judges rate them by. Exports: Impact, Bug, RUBRIC,
// parse_bugs, rank_bugs; deps: serde_json.
use serde_json::Value;

/// The one impact rubric every aic judge and challenger is given.
pub const RUBRIC: &str = "Rate each finding by its impact on what this change does:\n\
- high: this change leaves the code worse than before it: a reachable path it introduces or alters that, in normal use or for an actor who crosses a real permission boundary, gives wrong results, loses or corrupts data, breaks a security boundary, crashes, or breaks something that worked before.\n\
- medium: a real defect on a reachable but uncommon path whose effect is visible and recoverable.\n\
- low: everything else, including style, naming, docs, test coverage, an actor who already holds what the finding says they gain (the same user, the user's own configuration), environment faults such as clock skew or a full disk, code this change does not touch, a defect the code had before this change (including one this change reduces but does not remove), and any finding without a concrete trigger.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Impact { Low, Medium, High }

impl Impact {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self { Self::High => "high", Self::Medium => "medium", Self::Low => "low" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bug { pub impact: Impact, pub bug: String, pub trigger: String }

/// Parses judge bug objects. `None` when any entry lacks a valid impact or description.
/// A bug without a trigger is rated low whatever the judge said.
pub fn parse_bugs(values: &[Value]) -> Option<Vec<Bug>> {
    values.iter().map(parse_bug).collect()
}

fn parse_bug(value: &Value) -> Option<Bug> {
    let impact = Impact::parse(value.get("impact")?.as_str()?)?;
    let bug = value.get("bug")?.as_str()?.trim().to_owned();
    if bug.is_empty() {
        return None;
    }
    let trigger = value.get("trigger").and_then(Value::as_str).unwrap_or("").trim().to_owned();
    let impact = if trigger.is_empty() { Impact::Low } else { impact };
    Some(Bug { impact, bug, trigger })
}

/// Highest impact first; every high finding is kept and the rest are cut to `limit`.
pub fn rank_bugs(mut bugs: Vec<Bug>, limit: usize) -> Vec<Bug> {
    bugs.sort_by_key(|bug| std::cmp::Reverse(bug.impact));
    let high = bugs.iter().filter(|bug| bug.impact == Impact::High).count();
    bugs.truncate(high.max(limit));
    bugs
}

pub fn max_impact(bugs: &[Bug]) -> Option<Impact> {
    bugs.iter().map(|bug| bug.impact).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rubric_reserves_high_for_a_change_that_is_worse_than_before() {
        assert!(RUBRIC.contains("- high: this change leaves the code worse than before it"));
        assert!(RUBRIC.contains("including one this change reduces but does not remove"));
    }

    #[test]
    fn parses_bug_objects_and_forces_low_without_trigger() {
        let bugs = parse_bugs(&[
            json!({"impact": "HIGH", "bug": "drops rows", "trigger": "empty batch"}),
            json!({"impact": "high", "bug": "maybe races", "trigger": " "}),
        ]).expect("valid bugs");
        assert_eq!(bugs[0].impact, Impact::High);
        assert_eq!(bugs[1].impact, Impact::Low);
    }

    #[test]
    fn rejects_bugs_without_impact_or_text() {
        assert!(parse_bugs(&[json!("plain string bug")]).is_none());
        assert!(parse_bugs(&[json!({"impact": "severe", "bug": "x", "trigger": "y"})]).is_none());
        assert!(parse_bugs(&[json!({"impact": "low", "bug": "", "trigger": "y"})]).is_none());
    }

    #[test]
    fn ranking_keeps_high_findings_through_truncation() {
        let bug = |impact, text: &str| Bug { impact, bug: text.into(), trigger: "t".into() };
        let ranked = rank_bugs(vec![bug(Impact::Low, "a"), bug(Impact::Low, "b"), bug(Impact::Medium, "c"), bug(Impact::High, "d")], 2);
        assert_eq!(ranked.iter().map(|bug| bug.bug.as_str()).collect::<Vec<_>>(), ["d", "c"]);
        let all_high = rank_bugs(vec![bug(Impact::High, "a"), bug(Impact::High, "b"), bug(Impact::Low, "c"), bug(Impact::High, "d")], 2);
        assert_eq!(all_high.iter().map(|bug| bug.bug.as_str()).collect::<Vec<_>>(), ["a", "b", "d"]);
        assert_eq!(max_impact(&ranked), Some(Impact::High));
        assert_eq!(max_impact(&[]), None);
    }
}
