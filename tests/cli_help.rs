//! CLI integration smoke tests for scaffolded commands.
//! Exports: binary-level tests; deps: assert_cmd.

use assert_cmd::Command;
use std::error::Error;

fn stdout_for(args: &[&str]) -> Result<String, Box<dyn Error>> {
    let config_home = tempfile::tempdir()?;
    let output = Command::cargo_bin("aic")?
        .env("XDG_CONFIG_HOME", config_home.path())
        .args(args).output()?;
    assert!(output.status.success());
    Ok(String::from_utf8(output.stdout)?)
}

#[test]
fn root_help_lists_top_level_subcommands() -> Result<(), Box<dyn Error>> {
    let stdout = stdout_for(&["--help"])?;
    assert!(stdout.contains("init"));
    assert!(stdout.contains("audit"));
    assert!(stdout.contains("report"));
    assert!(stdout.contains("checklist"));

    Ok(())
}

#[test]
fn audit_help_shows_diff_option() -> Result<(), Box<dyn Error>> {
    let stdout = stdout_for(&["audit", "--help"])?;
    assert!(stdout.contains("--diff <path>"));
    assert!(stdout.contains("[task-id]"));

    Ok(())
}

#[test]
fn challenge_help_shows_report_diff_and_agent() -> Result<(), Box<dyn Error>> {
    let stdout = stdout_for(&["challenge", "--help"])?;
    for flag in ["--report <path>", "--diff <path>", "--agent <name>"] {
        assert!(stdout.contains(flag), "missing {flag}");
    }
    Ok(())
}

#[test]
fn init_stub_exits_successfully() -> Result<(), Box<dyn Error>> {
    let stdout = stdout_for(&["init"])?;
    assert!(stdout.contains("not yet implemented: init"));

    Ok(())
}
