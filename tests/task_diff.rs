//! Task audit process tests using real Git repositories and strict fake aid metadata.
//! Captures L2 context to verify exact diff inputs; deps: assert_cmd, serde_json, tempfile.
#![cfg(unix)]

use assert_cmd::Command;
use serde_json::{json, Value};
use std::{
    error::Error,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tempfile::{tempdir, TempDir};

type TestResult = Result<(), Box<dyn Error>>;

const FAKE_AID: &str = r#"#!/bin/sh
if [ "$#" = 3 ] && [ "$1" = show ] && [ "$2" = t-task ] && [ "$3" = --json ]; then
    cat "$RECORD/metadata.json"
    exit
fi
if [ "$1" = run ] && [ "$4" = --context ]; then
    cat "$5" > "$RECORD/context" || exit 9
    printf '%s\n' '{"verdict":"fail","reason":"fixture blocking rule","cited_lines":[]}'
    exit
fi
printf 'unsupported aid invocation\n' >&2
exit 8
"#;

struct Fixture {
    root: TempDir,
    repo: PathBuf,
    start: String,
}

fn git(repo: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .args(args)
        .output()?;
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn repository(repo: &Path) -> Result<String, Box<dyn Error>> {
    fs::create_dir(repo)?;
    git(repo, &["init"])?;
    fs::write(repo.join(".gitignore"), ".aic/\n")?;
    fs::write(repo.join("README.md"), "base\n")?;
    git(repo, &["add", "."])?;
    git(repo, &["commit", "-m", "Initial content"])?;
    git(repo, &["rev-parse", "HEAD"])
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempdir()?;
        let repo = root.path().join("repo");
        let start = repository(&repo)?;
        fs::create_dir_all(repo.join(".aic/rules"))?;
        fs::write(
            repo.join(".aic/config.toml"),
            "[layers.l0]\ncommands=[]\nsecret_scan=false\naddress_verify=false\n\
            [layers.l1]\ncommands=[]\n[layers.l2]\nenabled=true\n[layers.l3]\nenabled=false\n",
        )?;
        fs::write(
            repo.join(".aic/rules/check.md"),
            "---\nid: check\nseverity: blocking\nscope: ['**']\nlayer: l2\n---\n# Check\nCheck the diff.\n",
        )?;
        fs::create_dir(root.path().join("bin"))?;
        let aid = root.path().join("bin/aid");
        fs::write(&aid, FAKE_AID)?;
        fs::set_permissions(&aid, fs::Permissions::from_mode(0o755))?;
        Ok(Self { root, repo, start })
    }

    fn metadata(&self, final_head: Option<&str>, checkout: Option<&Path>) -> TestResult {
        self.write_metadata(json!({
            "start_sha": self.start, "final_head_sha": final_head, "worktree_path": checkout,
        }))
    }

    fn write_metadata(&self, value: Value) -> TestResult {
        fs::write(
            self.root.path().join("metadata.json"),
            serde_json::to_vec(&value)?,
        )?;
        Ok(())
    }

    fn audit(&self) -> Result<Command, Box<dyn Error>> {
        let mut command = Command::cargo_bin("aic")?;
        command
            .current_dir(&self.repo)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("RECORD", self.root.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.path().join("bin").display(),
                    std::env::var("PATH")?
                ),
            )
            .args(["audit", "t-task"]);
        Ok(command)
    }

    fn context(&self) -> Result<String, Box<dyn Error>> {
        Ok(fs::read_to_string(self.root.path().join("context"))?)
    }

    fn commit(&self, text: &str) -> Result<String, Box<dyn Error>> {
        fs::write(self.repo.join("README.md"), text)?;
        git(&self.repo, &["add", "README.md"])?;
        git(&self.repo, &["commit", "-m", "Change content"])?;
        git(&self.repo, &["rev-parse", "HEAD"])
    }
}

#[test]
fn in_place_committed_changes_reach_l2() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.commit("base\ncommitted-task-line\n")?;
    fixture.metadata(None, None)?;
    fixture.audit()?.assert().code(1);
    assert!(fixture.context()?.contains("+committed-task-line"));
    let result = fs::read_to_string(fixture.repo.join(".aic/audits/t-task/result.json"))?;
    assert!(result.contains("FailBlocking"));
    Ok(())
}

#[test]
fn matching_final_head_includes_staged_unstaged_and_untracked_without_index_changes() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join("README.md"), "base\nstaged-line\n")?;
    git(&fixture.repo, &["add", "README.md"])?;
    fs::write(
        fixture.repo.join("README.md"),
        "base\nstaged-line\nunstaged-line\n",
    )?;
    fs::write(fixture.repo.join("new file.txt"), "untracked-line\n")?;
    let index = git(&fixture.repo, &["diff", "--cached"])?;
    fixture.metadata(Some(&fixture.start), None)?;
    fs::create_dir_all(fixture.repo.join(".aic/audits/old"))?;
    fs::write(fixture.repo.join(".aic/audits/old/report.md"), "earlier-audit-report\n")?;
    fixture.audit()?.assert().code(1);
    let context = fixture.context()?;
    for line in ["+staged-line", "+unstaged-line", "+untracked-line"] {
        assert!(!fixture.context()?.contains("earlier-audit-report"));
    assert!(context.contains(line), "missing {line}: {context}");
    }
    assert_eq!(git(&fixture.repo, &["diff", "--cached"])?, index);
    assert!(git(&fixture.repo, &["ls-files", "--others", "--exclude-standard"])?.contains("new file.txt"));
    Ok(())
}

#[test]
fn moved_head_excludes_later_commits_and_working_changes() -> TestResult {
    let fixture = Fixture::new()?;
    let final_head = fixture.commit("base\ntask-result-line\n")?;
    fixture.commit("base\ntask-result-line\nlater-commit-line\n")?;
    fs::write(fixture.repo.join("README.md"), "later-working-line\n")?;
    fs::write(fixture.repo.join("later.txt"), "later-untracked-line\n")?;
    fixture.metadata(Some(&final_head), None)?;
    fixture.audit()?.assert().code(1);
    let context = fixture.context()?;
    assert!(context.contains("+task-result-line"));
    for excluded in ["later-commit-line", "later-working-line", "later-untracked-line"] {
        assert!(!context.contains(excluded));
    }
    Ok(())
}

#[test]
fn recorded_checkout_is_used() -> TestResult {
    let mut fixture = Fixture::new()?;
    let checkout = fixture.root.path().join("checkout");
    fixture.start = repository(&checkout)?;
    fs::write(checkout.join("README.md"), "base\ncheckout-task-line\n")?;
    fixture.metadata(None, Some(&checkout))?;
    fixture.audit()?.assert().code(1);
    assert!(fixture.context()?.contains("+checkout-task-line"));
    Ok(())
}

#[test]
fn missing_recorded_checkout_is_runtime_error() -> TestResult {
    let fixture = Fixture::new()?;
    let checkout = fixture.root.path().join("gone");
    fixture.metadata(None, Some(&checkout))?;
    let output = fixture.audit()?.assert().code(4).get_output().stderr.clone();
    assert!(String::from_utf8(output)?.contains(&checkout.display().to_string()));
    Ok(())
}

#[test]
fn null_or_absent_start_sha_is_runtime_error() -> TestResult {
    let fixture = Fixture::new()?;
    for metadata in [json!({"start_sha": null}), json!({})] {
        fixture.write_metadata(metadata)?;
        let output = fixture.audit()?.assert().code(4).get_output().stderr.clone();
        assert!(String::from_utf8(output)?.contains("start_sha"));
    }
    Ok(())
}

#[test]
fn invalid_git_revision_is_runtime_error() -> TestResult {
    let fixture = Fixture::new()?;
    for metadata in [
        json!({"start_sha": "not-a-revision"}),
        json!({"start_sha": fixture.start, "final_head_sha": "not-a-revision"}),
    ] {
        fixture.write_metadata(metadata)?;
        fixture.audit()?.assert().code(4);
    }
    Ok(())
}

#[test]
fn unchanged_task_is_empty_diff_pass_without_judge() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.metadata(Some(&fixture.start), None)?;
    let output = fixture.audit()?.assert().success().get_output().stdout.clone();
    assert!(String::from_utf8(output)?.contains("empty diff — no code to audit"));
    assert!(!fixture.root.path().join("context").exists());
    Ok(())
}

#[test]
fn absent_final_sha_includes_untracked_only_changes() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join("new.txt"), "untracked-only-line\n")?;
    fixture.write_metadata(json!({"start_sha": fixture.start}))?;
    fixture.audit()?.assert().code(1);
    assert!(fixture.context()?.contains("+untracked-only-line"));
    Ok(())
}

#[test]
fn diff_ignores_external_command_and_color_configuration() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.commit("base\nplain-diff-line\n")?;
    git(
        &fixture.repo,
        &["config", "diff.external", "missing-diff-command"],
    )?;
    git(&fixture.repo, &["config", "color.ui", "always"])?;
    fixture.metadata(None, None)?;
    fixture.audit()?.assert().code(1);
    let context = fixture.context()?;
    assert!(context.contains("+plain-diff-line"));
    assert!(!context.contains('\u{1b}'));
    Ok(())
}
