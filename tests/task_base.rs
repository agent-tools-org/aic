//! Base commit audit regressions through the real CLI and temporary git repos.
//! Exercises tampered config, rules and unavailable base errors.
#![cfg(unix)]
use assert_cmd::Command;
use std::fs;
#[path = "support/task_fixture.rs"]
mod support;
use support::*;

#[test]
fn last_output_line_names_the_written_report() -> TestResult {
    let fixture = Fixture::new()?;
    let output = fixture.audit("t-task")?.output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let path = stdout.lines().last().and_then(|line| line.strip_prefix("report: "));
    let path = path.ok_or("last line does not name the report")?;
    assert!(std::path::Path::new(path).is_absolute(), "{path}");
    assert!(!fs::read_to_string(path)?.is_empty());
    Ok(())
}

#[test]
fn task_cannot_disable_base_l2_or_weaken_rule() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.join(".aic/config.toml"),
        "[layers.l2]\nenabled=false\n",
    )?;
    fs::write(fixture.repo.join(".aic/rules/check.md"), "weakened")?;
    fixture.audit("t-task")?.assert().code(1);
    fixture.assert_temp_removed()?;
    assert!(fs::read_to_string(fixture.root.path().join("context"))?.contains("Check the diff."));
    Ok(())
}

#[test]
fn deleted_base_rule_still_applies() -> TestResult {
    let fixture = Fixture::new()?;
    fs::remove_file(fixture.repo.join(".aic/rules/check.md"))?;
    fixture.audit("t-task")?.assert().code(1);
    Ok(())
}

#[test]
fn added_rule_is_not_applied() -> TestResult {
    let fixture = Fixture::new()?;
    fs::remove_file(fixture.repo.join(".aic/rules/check.md"))?;
    fixture.commit()?;
    fs::write(
        fixture.repo.join(".aic/rules/added.md"),
        "---\nid: added\nseverity: blocking\nscope: ['**']\nlayer: l2\n---\n# Added\n",
    )?;
    fixture.audit("t-task")?.assert().success();
    assert!(!fixture.root.path().join("context").exists());
    Ok(())
}

#[test]
fn malformed_or_unreadable_base_never_reads_checkout_config() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join(".aic/config.toml"), "invalid toml !")?;
    for base in ["bad\n", "0000000000000000000000000000000000000000\n"] {
        fs::write(fixture.root.path().join("base"), base)?;
        let output = fixture
            .audit("t-task")?
            .assert()
            .code(4)
            .get_output()
            .stderr
            .clone();
        let stderr = String::from_utf8(output)?;
        assert!(stderr.contains("newer aid"));
        assert!(!stderr.contains("parse"));
    }
    Ok(())
}

#[test]
fn rules_outside_aic_are_pinned_and_escape_is_rejected() -> TestResult {
    let fixture = Fixture::new()?;
    fs::rename(fixture.repo.join(".aic/rules"), fixture.repo.join("policy"))?;
    let path = fixture.repo.join(".aic/config.toml");
    let original = fs::read_to_string(&path)?;
    fs::write(
        &path,
        original.replace("enabled=true", "enabled=true\nrules_dir='policy'"),
    )?;
    fixture.commit()?;
    fs::remove_file(fixture.repo.join("policy/check.md"))?;
    fixture.audit("t-task")?.assert().code(1);
    fs::write(
        &path,
        original.replace("enabled=true", "enabled=true\nrules_dir='../escape'"),
    )?;
    fixture.commit()?;
    fixture.audit("t-task")?.assert().code(4);
    Ok(())
}

#[test]
fn explicit_config_uses_checkout_without_base_lookup() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.path().join("base"), "bad")?;
    let mut command = Command::cargo_bin("aic")?;
    command
        .current_dir(&fixture.repo)
        .env("XDG_CONFIG_HOME", fixture.root.path().join("config"))
        .env("TMPDIR", fixture.root.path().join("tmp"))
        .env("RECORD", fixture.root.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fixture.root.path().join("bin").display(),
                std::env::var("PATH")?
            ),
        )
        .args(["--config", ".aic/config.toml", "audit", "t-task"])
        .assert()
        .code(1);
    Ok(())
}

#[test]
fn absent_base_config_ignores_added_checkout_config_and_rules() -> TestResult {
    let fixture = Fixture::new()?;
    fs::remove_file(fixture.repo.join(".aic/config.toml"))?;
    fs::remove_file(fixture.repo.join(".aic/rules/check.md"))?;
    fixture.commit()?;
    fs::write(fixture.repo.join(".aic/config.toml"), "invalid !")?;
    fs::create_dir_all(fixture.root.path().join("config/aic"))?;
    fs::write(
        fixture.root.path().join("config/aic/config.toml"),
        "[layers.l3]\nenabled=false\n",
    )?;
    fixture.audit("t-task")?.assert().success();
    Ok(())
}

#[test]
fn deleted_cargo_manifest_still_selects_cargo_defaults() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.join("Cargo.toml"),
        "[package]\nname='fixture'\n",
    )?;
    fs::write(
        fixture.repo.join(".aic/config.toml"),
        "[layers.l0]\nsecret_scan=false\naddress_verify=false\n[layers.l3]\nenabled=false\n",
    )?;
    fixture.commit()?;
    fs::remove_file(fixture.repo.join("Cargo.toml"))?;
    let cargo = fixture.root.path().join("bin/cargo");
    fs::write(&cargo, "#!/bin/sh\nprintf cargo-default >&2\nexit 1\n")?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(cargo, fs::Permissions::from_mode(0o755))?;
    let output = fixture
        .audit("t-task")?
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(output)?.contains("cargo-default"));
    Ok(())
}

#[test]
fn config_uses_recorded_start_even_when_patch_is_against_an_older_commit() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.repo.join(".aic/config.toml");
    let enabled = fs::read_to_string(&path)?;
    fs::write(&path, enabled.replace("enabled=true", "enabled=false"))?;
    fixture.commit()?;
    let older = fs::read_to_string(fixture.root.path().join("base"))?;
    fs::write(&path, &enabled)?;
    fs::create_dir_all(fixture.repo.join("src"))?;
    fs::write(fixture.repo.join("src/task.rs"), "start-line\n")?;
    fixture.commit()?;
    fs::write(fixture.repo.join("src/task.rs"), "delivered-line\n")?;
    let patch = std::process::Command::new("git")
        .args(["diff", older.trim()])
        .current_dir(&fixture.repo)
        .output()?;
    assert!(patch.status.success());
    fs::write(fixture.root.path().join("patch"), patch.stdout)?;
    fs::write(&path, enabled.replace("enabled=true", "enabled=false"))?;
    fixture.audit("t-task")?.assert().code(1);
    assert!(fs::read_to_string(fixture.root.path().join("context"))?.contains("Check the diff."));
    fixture.assert_temp_removed()
}

#[test]
fn unrelated_non_utf8_base_filename_does_not_break_audit() -> TestResult {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let fixture = Fixture::new()?;
    let name = OsString::from_vec(vec![b'u', 0xff]);
    fs::write(fixture.repo.join(name), "unrelated")?;
    fixture.commit()?;
    fixture.audit("t-task")?.assert().code(1);
    fixture.assert_temp_removed()
}

#[test]
fn absolute_user_rules_remain_external() -> TestResult {
    let fixture = Fixture::new()?;
    let external = fixture.root.path().join("user-rules");
    fs::rename(fixture.repo.join(".aic/rules"), &external)?;
    fixture.commit()?;
    fs::create_dir_all(fixture.root.path().join("config/aic"))?;
    fs::write(
        fixture.root.path().join("config/aic/config.toml"),
        format!("[layers.l2]\nrules_dir='{}'\n", external.display()),
    )?;
    fixture.audit("t-task")?.assert().code(1);
    assert!(fs::read_to_string(fixture.root.path().join("context"))?.contains("Check the diff."));
    fixture.assert_temp_removed()
}

#[test]
fn config_error_removes_snapshot_before_main_exits() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join(".aic/config.toml"), "invalid !")?;
    fixture.commit()?;
    fixture.audit("t-task")?.assert().code(4);
    fixture.assert_temp_removed()
}

#[test]
fn start_commit_failure_never_reads_checkout_config() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join(".aic/config.toml"), "invalid toml !")?;
    fs::write(
        fixture.root.path().join("bin/aid"),
        "#!/bin/sh\nif [ \"$3\" = --patch ]; then cat \"$RECORD/patch\"; exit; fi\n\
        if [ \"$3\" = --start-commit ]; then echo start-failed >&2; exit 9; fi\nexit 8\n",
    )?;
    let output = fixture
        .audit("t-task")?
        .assert()
        .code(4)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(output)?;
    assert!(stderr.contains("aid show --start-commit failed: start-failed"));
    assert!(stderr.contains("newer aid"));
    assert!(!stderr.contains("parse"));
    assert!(!fixture.repo.join(".aic/audits").exists());
    fixture.assert_temp_removed()
}

#[test]
fn git_replace_cannot_disable_start_commit_audit() -> TestResult {
    let fixture = Fixture::new()?;
    let start = fs::read_to_string(fixture.root.path().join("base"))?;
    let path = fixture.repo.join(".aic/config.toml");
    let config = fs::read_to_string(&path)?;
    fs::write(&path, config.replace("enabled=true", "enabled=false"))?;
    fixture.commit()?;
    let forged = fs::read_to_string(fixture.root.path().join("base"))?;
    let status = std::process::Command::new("git")
        .args(["replace", start.trim(), forged.trim()])
        .current_dir(&fixture.repo)
        .status()?;
    assert!(status.success());
    fs::write(fixture.root.path().join("base"), start)?;
    fixture.audit("t-task")?.assert().code(1);
    assert!(fs::read_to_string(fixture.root.path().join("context"))?.contains("Check the diff."));
    fixture.assert_temp_removed()
}

#[test]
fn rules_dir_cannot_normalize_to_repository_root() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.repo.join(".aic/config.toml");
    let config = fs::read_to_string(&path)?;
    for rules in [".", "policy/.."] {
        fs::write(
            &path,
            config.replace(
                "enabled=true",
                &format!("enabled=true\nrules_dir='{rules}'"),
            ),
        )?;
        fixture.commit()?;
        let output = fixture
            .audit("t-task")?
            .assert()
            .code(4)
            .get_output()
            .stderr
            .clone();
        assert!(String::from_utf8(output)?.contains("must not normalize to the repository root"));
        fixture.assert_temp_removed()?;
    }
    Ok(())
}
