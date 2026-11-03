//! Task audit process tests with strict fake aid patch collection and judge responses.
//! Verifies patch context, rule matching, runtime errors and temporary-file cleanup.
#![cfg(unix)]

use assert_cmd::Command;
use std::{error::Error, fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tempfile::{tempdir, TempDir};

type TestResult = Result<(), Box<dyn Error>>;

const PATCH: &str = "diff --git a/src/task.rs b/src/task.rs\n--- a/src/task.rs\n\
    +++ b/src/task.rs\n@@ -1 +1 @@\n-old-line\n+task-patch-line\n";
const FAKE_AID: &str = r#"#!/bin/sh
if [ "$#" = 3 ] && [ "$1" = show ] && [ "$2" = t-task ] && [ "$3" = --patch ]; then
    pwd > "$RECORD/show-cwd"
    if [ -f "$RECORD/fail" ]; then
        printf 'unknown flag: --patch\n' >&2
        exit 8
    fi
    cat "$RECORD/patch"
    exit
fi
if [ "$1" = run ] && [ "$4" = --context ]; then
    cat "$5" > "$RECORD/context" || exit 9
    ls -A "$TMPDIR" > "$RECORD/live-temp-files"
    printf '%s\n' '{"verdict":"fail","reason":"fixture blocking rule","cited_lines":[]}'
    exit
fi
printf 'unsupported aid invocation\n' >&2
exit 8
"#;

struct Fixture {
    root: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempdir()?;
        let repo = root.path().join("repo");
        fs::create_dir_all(repo.join(".aic/rules"))?;
        fs::write(
            repo.join(".aic/config.toml"),
            "[layers.l0]\ncommands=[]\nsecret_scan=false\naddress_verify=false\n\
            [layers.l1]\ncommands=[]\n[layers.l2]\nenabled=true\n[layers.l3]\nenabled=false\n",
        )?;
        fs::write(
            repo.join(".aic/rules/check.md"),
            "---\nid: check\nseverity: blocking\nscope: ['src/**/*.rs']\nlayer: l2\n\
            ---\n# Check\nCheck the diff.\n",
        )?;
        fs::create_dir(root.path().join("bin"))?;
        fs::create_dir(root.path().join("tmp"))?;
        let aid = root.path().join("bin/aid");
        fs::write(&aid, FAKE_AID)?;
        fs::set_permissions(&aid, fs::Permissions::from_mode(0o755))?;
        fs::write(root.path().join("patch"), PATCH)?;
        Ok(Self { root, repo })
    }

    fn audit(&self, task_id: &str) -> Result<Command, Box<dyn Error>> {
        let mut command = Command::cargo_bin("aic")?;
        command
            .current_dir(&self.repo)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("RECORD", self.root.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.path().join("bin").display(),
                    std::env::var("PATH")?
                ),
            )
            .args(["audit", task_id]);
        Ok(command)
    }

    fn assert_temp_removed(&self) -> TestResult {
        assert_eq!(fs::read_dir(self.root.path().join("tmp"))?.count(), 0);
        Ok(())
    }
}

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
    let output = fixture.audit("x/../../escaped")?.assert().code(4).get_output().stderr.clone();
    assert!(String::from_utf8(output)?.contains("invalid task id"));
    assert!(!fixture.root.path().join("show-cwd").exists(), "aid must not run");
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
    fixture.audit("t-task")?.assert().code(1);
    assert!(fs::read_to_string(fixture.root.path().join("context"))?.contains("Check café."));
    fixture.assert_temp_removed()
}
