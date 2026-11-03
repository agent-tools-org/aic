//! Judge CLI compatibility at the subprocess boundary.
//! Uses isolated fake aid executables and real L2/dispatcher calls.
#![cfg(unix)]
use aic::config::L2;
use aic::layers::aid_dispatch::{AidDispatcher, RealAidDispatcher};
use aic::layers::l2::L2Runner;
use std::{error::Error, fs, os::unix::fs::PermissionsExt, path::Path};
use tempfile::tempdir;

const FAKE_AID: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$RECORD/args"
printf '%s' "$4" > "$RECORD/path"
cat "$4" > "$RECORD/prompt" || exit 9
if [ "$FAIL_JUDGE" = 1 ]; then exit 7; fi
printf '{"verdict":"pass","reason":"ok","cited_lines":[]}'
"#;

#[test]
fn judge_process_contract() -> Result<(), Box<dyn Error>> {
    if let Ok(root) = std::env::var("JUDGE_CHILD") {
        return child_calls(Path::new(&root));
    }
    let dir = tempdir()?;
    let aid = dir.path().join("aid");
    fs::write(&aid, FAKE_AID)?;
    fs::set_permissions(&aid, fs::Permissions::from_mode(0o755))?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "judge_process_contract", "--nocapture"])
        .env("JUDGE_CHILD", dir.path())
        .env("RECORD", dir.path())
        .env(
            "PATH",
            format!("{}:{}", dir.path().display(), std::env::var("PATH")?),
        )
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn child_calls(root: &Path) -> Result<(), Box<dyn Error>> {
    let prompt = "large diff\n".repeat(20_000);
    RealAidDispatcher.dispatch("custom-agent", &prompt)?;
    assert_eq!(fs::read_to_string(root.join("prompt"))?, prompt);
    let path = fs::read_to_string(root.join("path"))?;
    assert!(!Path::new(&path).exists());
    let expected = format!(
        "run\ncustom-agent\n--prompt-file\n{path}\n--read-only\n--no-audit\n--verify\nnone\n\
         --difficulty\nsimple\n--budget\nstandard\n--urgency\nnormal\n--rigor\nstandard\n"
    );
    assert_eq!(fs::read_to_string(root.join("args"))?, expected);
    std::env::set_var("FAIL_JUDGE", "1");
    assert!(RealAidDispatcher.dispatch("custom-agent", &prompt).is_err());
    let path = fs::read_to_string(root.join("path"))?;
    assert!(!Path::new(&path).exists());
    std::env::remove_var("FAIL_JUDGE");
    fs::write(
        root.join("rule.md"),
        "---\nid: custom\nseverity: blocking\nscope: ['src/**']\nlayer: l2\n---\n# Check\nPass.",
    )?;
    let config = L2 {
        model: "configured-agent".into(),
        ..L2::default()
    };
    L2Runner::new(&config, root.to_owned()).run("diff", &["src/lib.rs".into()], &[]);
    assert!(fs::read_to_string(root.join("args"))?.starts_with("run\nconfigured-agent\n"));
    Ok(())
}

#[test]
fn missing_aid_cleans_prompt_file() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    fs::create_dir(dir.path().join("tmp"))?;
    fs::write(
        dir.path().join("diff.patch"),
        "diff --git a/a b/a\n+++ b/a\n@@\n+x\n",
    )?;
    fs::create_dir(dir.path().join(".aic"))?;
    fs::write(
        dir.path().join(".aic/config.toml"),
        "[layers.l0]\nsecret_scan=false\naddress_verify=false\n[layers.l2]\nenabled=false\n",
    )?;
    assert_cmd::Command::cargo_bin("aic")?
        .current_dir(dir.path())
        .env("PATH", dir.path())
        .env("TMPDIR", dir.path().join("tmp"))
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .code(3);
    assert_eq!(fs::read_dir(dir.path().join("tmp"))?.count(), 0);
    Ok(())
}
