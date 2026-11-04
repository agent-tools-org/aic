//! Base commit audit regressions through the real CLI and temporary git repos.
//! Exercises tampered config, rules and unavailable base errors.
#![cfg(unix)]
use assert_cmd::Command;
use std::fs;
#[path = "support/task_fixture.rs"]
mod support;
use support::*;

#[test]
fn task_cannot_disable_base_l2_or_weaken_rule() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.join(".aic/config.toml"),
        "[layers.l2]\nenabled=false\n",
    )?;
    fs::write(fixture.repo.join(".aic/rules/check.md"), "weakened")?;
    fixture.audit("t-task")?.assert().code(1);
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
    fs::write(fixture.root.path().join("bin/aid"), "#!/bin/sh\nexit 9\n")?;
    fixture.audit("t-task")?.assert().code(4);
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
