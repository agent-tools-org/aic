//! Task audit process tests with strict fake aid patch collection and judge responses.
//! Verifies patch context, rule matching, runtime errors and temporary-file cleanup.
#![cfg(unix)]

use std::{fs, path::PathBuf};

#[path = "support/task_fixture.rs"]
mod support;
use support::*;

#[test]
fn patch_lines_reach_judge_and_touched_files_match_rules() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.audit("t-task")?.assert().code(1);
    let context = fs::read_to_string(fixture.root.path().join("context"))?;
    assert!(context.contains(PATCH));
    assert!(context.contains("Check the diff."));
    let cwd = fs::read_to_string(fixture.root.path().join("show-cwd"))?;
    assert_eq!(
        PathBuf::from(cwd.trim()).canonicalize()?,
        fixture.repo.canonicalize()?
    );
    assert!(
        !fs::read_to_string(fixture.root.path().join("live-temp-files"))?
            .trim()
            .is_empty()
    );
    fixture.assert_temp_removed()
}

#[test]
fn unmatched_touched_files_do_not_call_judge() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.root.path().join("patch"),
        PATCH.replace("src/task.rs", "README.md"),
    )?;
    fixture.audit("t-task")?.assert().success();
    assert!(!fixture.root.path().join("context").exists());
    fixture.assert_temp_removed()
}

#[test]
fn aid_show_failure_is_runtime_error_with_stderr_and_upgrade_hint() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("fail"), "")?;
    let output = fixture
        .audit("t-task")?
        .assert()
        .code(4)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(output)?;
    assert!(stderr.contains("unknown flag: --patch"));
    assert!(stderr.contains("requires an aid release with `aid show --patch` support"));
    assert!(!fixture.root.path().join("context").exists());
    fixture.assert_temp_removed()
}

#[test]
fn empty_stdout_passes_without_judge() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("patch"), "")?;
    let output = fixture
        .audit("t-task")?
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(output)?.contains("empty diff — no code to audit"));
    assert!(!fixture.root.path().join("context").exists());
    fixture.assert_temp_removed()
}

#[test]
fn invalid_utf8_patch_reaches_judge_instead_of_runtime_error() -> TestResult {
    let fixture = Fixture::new()?;
    let mut patch = PATCH.as_bytes().to_vec();
    patch.push(0xff);
    fs::write(fixture.root.path().join("patch"), patch)?;
    fixture.audit("t-task")?.assert().code(1);
    let context = fs::read_to_string(fixture.root.path().join("context"))?;
    assert!(context.contains('\u{fffd}'));
    fixture.assert_temp_removed()
}

#[test]
fn traversal_task_id_is_refused_before_any_write() -> TestResult {
    let fixture = Fixture::new()?;
    let output = fixture
        .audit("x/../../escaped")?
        .assert()
        .code(4)
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8(output)?.contains("invalid task id"));
    assert!(
        !fixture.root.path().join("show-cwd").exists(),
        "aid must not run"
    );
    assert!(!fixture.repo.join(".aic/audits").exists());
    assert!(!fixture.repo.join(".aic/escaped").exists());
    Ok(())
}

#[test]
fn quoted_non_ascii_path_matches_scoped_rule_through_aid() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.join(".aic/rules/check.md"),
        "---\nid: check\nseverity: blocking\nscope: ['src/café.rs']\nlayer: l2\n---\n# Check\nCheck café.\n",
    )?;
    fs::write(
        fixture.root.path().join("patch"),
        "diff --git \"a/src/caf\\303\\251.rs\" \"b/src/caf\\303\\251.rs\"\n\
        --- \"a/src/caf\\303\\251.rs\"\n+++ \"b/src/caf\\303\\251.rs\"\n@@\n+x\n",
    )?;
    fixture.commit()?;
    fixture.audit("t-task")?.assert().code(1);
    assert!(fs::read_to_string(fixture.root.path().join("context"))?.contains("Check café."));
    fixture.assert_temp_removed()
}
