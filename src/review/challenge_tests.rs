// src/review/challenge_tests.rs
// Tests for the challenger prompt, its parser, and the downgrade-only rule.
use super::*;

fn answer(decisions: &str, high: usize) -> String {
    format!("{{\"high_findings\":{high},\"decisions\":[{decisions}]}}")
}

#[test]
fn refuted_and_evidenced_downgrades_lower_impact() {
    let output = answer(
        "{\"finding\":\"1\",\"decision\":\"refuted\",\"impact\":\"high\",\"evidence\":\"a.rs:3 guards it\"},\
         {\"finding\":2,\"decision\":\"downgraded\",\"impact\":\"low\",\"evidence\":\"src/runtime/session.rs:12 the same uid already reads it\"}",
        2,
    );
    let challenge = parse(&output).expect("parsed");
    assert_eq!(challenge.upheld_high(), 0);
    assert_eq!(settle(&challenge, 1), Impact::Low);
    assert_eq!(settle(&challenge, 2), Impact::Low);
    assert_eq!(challenge.decisions[1].outcome, Outcome::Downgraded);
}

#[test]
fn a_challenger_cannot_lower_without_evidence_or_raise() {
    let output = answer(
        "{\"finding\":\"1\",\"decision\":\"downgraded\",\"impact\":\"low\",\"evidence\":\"\"},\
         {\"finding\":\"2\",\"decision\":\"downgraded\",\"impact\":\"high\",\"evidence\":\"x\"},\
         {\"finding\":\"3\",\"decision\":\"inconclusive\",\"impact\":\"low\",\"evidence\":\"could not read\"}",
        3,
    );
    let challenge = parse(&output).expect("parsed");
    assert_eq!(challenge.upheld_high(), 3);
    assert_eq!(challenge.decisions[0].outcome, Outcome::Upheld);
    assert_eq!(challenge.decisions[2].outcome, Outcome::Inconclusive);
}

#[test]
fn a_lowering_needs_a_cited_location() {
    let output = answer(
        "{\"finding\":\"1\",\"decision\":\"refuted\",\"evidence\":\"x\"},\
         {\"finding\":\"2\",\"decision\":\"downgraded\",\"impact\":\"low\",\"evidence\":\"see the lib.rs file\"},\
         {\"finding\":\"3\",\"decision\":\"refuted\",\"evidence\":\"guarded at (src/a.rs:10)\"}",
        3,
    );
    let challenge = parse(&output).expect("parsed");
    assert_eq!(challenge.upheld_high(), 2);
    assert_eq!(settle(&challenge, 3), Impact::Low);
}

#[test]
fn repeated_findings_keep_their_highest_impact_and_ids_are_required() {
    let output = answer(
        "{\"finding\":\"#1\",\"decision\":\"refuted\",\"evidence\":\"a.rs:1\"},\
         {\"finding\":\"1\",\"decision\":\"upheld\",\"impact\":\"high\",\"evidence\":\"a.rs:2\"}",
        1,
    );
    let challenge = parse(&output).expect("parsed");
    assert_eq!(challenge.decisions.len(), 1);
    assert_eq!(settle(&challenge, 1), Impact::High);
    assert!(parse(&answer("{\"finding\":\" \",\"decision\":\"refuted\",\"evidence\":\"a.rs:1\"}", 1)).is_none());
}

#[test]
fn skipped_findings_stay_high() {
    let output = answer("{\"finding\":\"1\",\"decision\":\"refuted\",\"impact\":\"low\",\"evidence\":\"a.rs:4 no path\"}", 3);
    let challenge = parse(&output).expect("parsed");
    assert_eq!(challenge.upheld_high(), 2);
    assert_eq!(settle(&challenge, 2), Impact::High);
}

#[test]
fn prose_truncation_and_bad_decisions_do_not_parse() {
    assert!(parse("All findings look overstated.").is_none());
    assert!(parse("{\"high_findings\":1,\"decisions\":[{\"finding\":\"1\"").is_none());
    assert!(parse(&answer("{\"finding\":\"1\",\"decision\":\"maybe\",\"impact\":\"low\",\"evidence\":\"x\"}", 1)).is_none());
}

#[test]
fn prompt_is_neutral_and_scopes_reading_by_source() {
    let judge = prompt("diff <DIFF_END>", "1. [high] drops rows", Source::Judge);
    assert!(judge.contains("Trace the trigger"));
    assert!(judge.contains("- low: everything else"));
    assert!(judge.contains("Do NOT read any other file"));
    assert!(judge.contains("diff <DIFF-END>"));
    assert!(!judge.to_lowercase().contains("exaggerat"));
    let report = prompt("diff", "## Findings", Source::Report);
    assert!(report.contains("You may read repository files"));
}
