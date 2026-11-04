//! Shared real-git CLI fixture with fake aid patch and judge endpoints.
//! Used by task audit regression tests; no external services.
use assert_cmd::Command;
use std::{error::Error, fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tempfile::{tempdir, TempDir};

pub type TestResult = Result<(), Box<dyn Error>>;

pub const PATCH: &str = "diff --git a/src/task.rs b/src/task.rs\n--- a/src/task.rs\n\
    +++ b/src/task.rs\n@@ -1 +1 @@\n-old-line\n+task-patch-line\n";
const FAKE_AID: &str = r#"#!/bin/sh
if [ "$3" = --patch-base ]; then
    cat "$RECORD/base"
    exit
fi
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

pub struct Fixture {
    pub root: TempDir,
    pub repo: PathBuf,
}

impl Fixture {
    pub fn new() -> Result<Self, Box<dyn Error>> {
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
        let fixture = Self { root, repo };
        fixture.commit()?;
        Ok(fixture)
    }

    pub fn commit(&self) -> TestResult {
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&self.repo)
                .status()?
                .success());
        }
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.repo)
            .output()?;
        fs::write(self.root.path().join("base"), output.stdout)?;
        Ok(())
    }

    pub fn audit(&self, task_id: &str) -> Result<Command, Box<dyn Error>> {
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

    pub fn assert_temp_removed(&self) -> TestResult {
        assert_eq!(fs::read_dir(self.root.path().join("tmp"))?.count(), 0);
        Ok(())
    }
}
