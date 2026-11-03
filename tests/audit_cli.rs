//! Audit CLI integration tests.
//! Exports: end-to-end audit command test; deps: assert_cmd, tempfile.

use assert_cmd::Command;
use std::{error::Error, fs};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

#[test]
fn audit_diff_persists_result_json() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    fs::create_dir_all(dir.path().join(".aic"))?;
    fs::write(
        dir.path().join(".aic").join("config.toml"),
        "[layers.l0]\ncommands = []\nsecret_scan = false\naddress_verify = false\n\n[layers.l1]\ncommands = [\"exit 0\"]\nmax_duration_secs = 5\n\n[layers.l2]\nenabled = false\n\n[layers.l3]\nenabled = false\n",
    )?;
    fs::write(
        dir.path().join("diff.patch"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+++ b/src/lib.rs\n@@\n+pub fn ok() {}\n",
    )?;

    let output = Command::cargo_bin("aic")?
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("isolated-config"))
        .args(["audit", "--diff", "diff.patch"])
        .output()?;

    assert!(output.status.success());
    assert!(dir.path().join(".aic").join("audits").join("diff").join("result.json").is_file());
    Ok(())
}

#[cfg(unix)]
#[test]
fn audit_diff_with_board_item_invokes_ai_board() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    let aic_dir = dir.path().join(".aic");
    let bin_dir = dir.path().join("bin");
    let log_path = dir.path().join("ai-board.log");
    fs::create_dir_all(&aic_dir)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(
        aic_dir.join("config.toml"),
        "[layers.l0]\ncommands = []\nsecret_scan = false\naddress_verify = false\n\n[layers.l1]\ncommands = [\"exit 0\"]\nmax_duration_secs = 5\n\n[layers.l2]\nenabled = false\n\n[layers.l3]\nenabled = false\n\n[integration]\naib_auto_block = true\n",
    )?;
    fs::write(
        dir.path().join("diff.patch"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+++ b/src/lib.rs\n@@\n+pub fn ok() {}\n",
    )?;
    let script_path = bin_dir.join("ai-board");
    fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$1\" = \"show\" ]; then\n  printf 'status: todo\\n'\nfi\n",
            log_path.display()
        ),
    )?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    let path = format!("{}:{}", bin_dir.display(), std::env::var("PATH")?);
    let output = Command::cargo_bin("aic")?
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("isolated-config"))
        .env("PATH", path)
        .args(["audit", "--diff", "diff.patch", "--board-item", "wi-fake"])
        .output()?;

    assert!(output.status.success());
    let log = fs::read_to_string(log_path)?;
    assert!(log.contains("show wi-fake"));
    assert!(log.contains("item attach wi-fake --artifact-type audit-report"));
    assert!(log.contains("update wi-fake --status review"));
    Ok(())
}
