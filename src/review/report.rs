// src/review/report.rs
// `aic challenge`: run the reverse guard over a free-form audit report and its diff.
// Exports: ChallengeOutcome, challenge_report, render; deps: review::challenge, layers::aid_dispatch.
use super::challenge::{self, Challenge, Source};
use crate::layers::aid_dispatch::AidDispatcher;
use crate::pipeline::MAX_JUDGE_DIFF_BYTES;
use anyhow::{bail, Context, Result};
use std::path::Path;

#[derive(Debug)]
pub enum ChallengeOutcome {
    /// The challenger answered; FIX holds only while a high finding is upheld.
    Settled(Challenge),
    /// The answer was not the challenge JSON, so the report's findings stand unchallenged.
    Inconclusive(String),
}

impl ChallengeOutcome {
    /// 0 when no high finding is upheld, 1 when one is, 3 when the challenge has no answer.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Settled(result) if result.upheld_high() == 0 => 0,
            Self::Settled(_) => 1,
            Self::Inconclusive(_) => 3,
        }
    }
}

pub fn challenge_report(report: &Path, diff: &Path, agent: &str, dispatcher: &dyn AidDispatcher) -> Result<ChallengeOutcome> {
    let report_text = std::fs::read_to_string(report).with_context(|| format!("read audit report {}", report.display()))?;
    let diff_text = std::fs::read_to_string(diff).with_context(|| format!("read diff {}", diff.display()))?;
    let size = report_text.len() + diff_text.len();
    if size > MAX_JUDGE_DIFF_BYTES {
        bail!("report and diff of {size} bytes exceed the {MAX_JUDGE_DIFF_BYTES} byte judge limit");
    }
    let response = dispatcher.dispatch(agent, &challenge::prompt(&diff_text, &report_text, Source::Report))?;
    Ok(match challenge::parse(response.stdout.trim()) {
        Some(result) => ChallengeOutcome::Settled(result),
        None => ChallengeOutcome::Inconclusive(preview(&response.stdout)),
    })
}

pub fn render(outcome: &ChallengeOutcome) -> String {
    let result = match outcome {
        ChallengeOutcome::Inconclusive(raw) => return format!("verdict: inconclusive (the challenger returned no challenge JSON; the report stands)\nanswer: {raw}\n"),
        ChallengeOutcome::Settled(result) => result,
    };
    let mut text = String::from("| finding | decision | impact | evidence |\n|---|---|---|---|\n");
    for decision in &result.decisions {
        text.push_str(&format!("| {} | {} | {} | {} |\n", cell(&decision.finding), decision.outcome.as_str(), decision.impact.as_str(), cell(&decision.evidence)));
    }
    let skipped = result.high_findings.saturating_sub(result.decisions.len());
    if skipped > 0 {
        text.push_str(&format!("\n{skipped} high finding(s) received no decision and stand.\n"));
    }
    let upheld = result.upheld_high();
    let verdict = if upheld == 0 { "SHIP (no high finding upheld)".to_owned() } else { format!("FIX ({upheld} high finding(s) upheld)") };
    text.push_str(&format!("\nverdict: {verdict}\n"));
    text
}

fn cell(text: &str) -> String { text.replace('|', "\\|").replace('\n', " ") }

fn preview(raw: &str) -> String { raw.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::aid_dispatch::AidResponse;

    struct Answer(&'static str);
    impl AidDispatcher for Answer {
        fn dispatch(&self, _agent: &str, prompt: &str) -> Result<AidResponse> {
            assert!(prompt.contains("You may read repository files") && prompt.contains("## Findings"));
            Ok(AidResponse { task_id: "t-c".into(), stdout: self.0.into(), cost_usd: 0.0, duration_ms: 1 })
        }
    }

    fn outcome(answer: &'static str) -> ChallengeOutcome {
        let dir = tempfile::tempdir().expect("tempdir");
        let (report, diff) = (dir.path().join("result.md"), dir.path().join("change.patch"));
        std::fs::write(&report, "## Findings\n1. **High** token leaks\n").expect("report");
        std::fs::write(&diff, "+let token = env();\n").expect("diff");
        challenge_report(&report, &diff, "codex", &Answer(answer)).expect("challenge")
    }

    #[test]
    fn downgraded_report_ships_and_upheld_report_fixes() {
        let downgraded = outcome("{\"high_findings\":1,\"decisions\":[{\"finding\":\"token leaks\",\"decision\":\"downgraded\",\"impact\":\"low\",\"evidence\":\"only the same uid | can read it\"}]}");
        assert_eq!(downgraded.exit_code(), 0);
        let text = render(&downgraded);
        assert!(text.contains("| token leaks | downgraded | low | only the same uid \\| can read it |"));
        assert!(text.contains("verdict: SHIP"));
        let upheld = outcome("{\"high_findings\":2,\"decisions\":[{\"finding\":\"token leaks\",\"decision\":\"refuted\",\"impact\":\"low\",\"evidence\":\"a.rs:1\"}]}");
        assert_eq!(upheld.exit_code(), 1);
        assert!(render(&upheld).contains("1 high finding(s) received no decision and stand."));
    }

    #[test]
    fn an_unparsed_answer_is_inconclusive() {
        let unparsed = outcome("The findings seem overstated.");
        assert_eq!(unparsed.exit_code(), 3);
        assert!(render(&unparsed).starts_with("verdict: inconclusive"));
    }
}
