//! Process-boundary coverage for user configuration and runtime failures.
//! Uses isolated HOME/XDG directories and fake executables; deps: assert_cmd, tempfile.
#![cfg(unix)]

use assert_cmd::Command;
use std::{error::Error, fs, os::unix::fs::PermissionsExt, path::Path};
use tempfile::{tempdir, TempDir};

type TestResult = Result<(), Box<dyn Error>>;

const USER_CONFIG: &str = "[layers.l0]\ncommands=[]\nsecret_scan=false\naddress_verify=false\n\
    [layers.l1]\ncommands=[]\n[layers.l2]\nenabled=false\n[layers.l3]\nenabled=false\n";

fn executable(path: &Path, contents: &str) -> TestResult {
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn fixture() -> Result<TempDir, Box<dyn Error>> {
    let dir = tempdir()?;
    for folder in ["xdg/aic", "home/.config/aic", ".aic", "bin"] {
        fs::create_dir_all(dir.path().join(folder))?;
    }
    fs::write(dir.path().join("Cargo.toml"), "[package]\n")?;
    fs::write(
        dir.path().join("diff.patch"),
        "diff --git a/a b/a\n+++ b/a\n@@\n+x\n",
    )?;
    executable(
        &dir.path().join("bin/cargo"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CALLS\"\n",
    )?;
    Ok(dir)
}

fn command(root: &Path) -> Result<Command, Box<dyn Error>> {
    let mut command = Command::cargo_bin("aic")?;
    command
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"))
        .env("CALLS", root.join("calls"))
        .env(
            "PATH",
            format!("{}:{}", root.join("bin").display(), std::env::var("PATH")?),
        );
    Ok(command)
}

#[test]
fn missing_task_is_runtime_error() -> TestResult {
    let dir = fixture()?;
    executable(
        &dir.path().join("bin/aid"),
        "#!/bin/sh\n[ \"$1\" = show ] && exit 9\nexit 0\n",
    )?;
    let output = command(dir.path())?
        .args(["audit", "t-missing"])
        .assert()
        .code(4)
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8(output)?.contains("Error: aid show t-missing --diff failed"));
    Ok(())
}

#[test]
fn xdg_empty_commands_skip_cargo_and_project_arrays_replace_them() -> TestResult {
    let dir = fixture()?;
    fs::write(dir.path().join("xdg/aic/config.toml"), USER_CONFIG)?;
    // HOME would block the audit if selected instead of XDG.
    fs::write(
        dir.path().join("home/.config/aic/config.toml"),
        "[layers.l0]\ncommands=['exit 8']\n",
    )?;
    command(dir.path())?
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .success();
    assert!(!dir.path().join("calls").exists());
    fs::write(
        dir.path().join(".aic/config.toml"),
        "[layers.l0]\ncommands=['cargo project-check']\n[layers.l1]\ncommands=['cargo project-test']\n",
    )?;
    command(dir.path())?
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(dir.path().join("calls"))?,
        "project-check\nproject-test\n"
    );
    Ok(())
}

#[test]
fn home_config_is_used_when_xdg_is_unset() -> TestResult {
    let dir = fixture()?;
    fs::write(dir.path().join("home/.config/aic/config.toml"), USER_CONFIG)?;
    command(dir.path())?
        .env_remove("XDG_CONFIG_HOME")
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .success();
    assert!(!dir.path().join("calls").exists());
    Ok(())
}

#[test]
fn malformed_user_config_names_its_path() -> TestResult {
    let dir = fixture()?;
    let path = dir.path().join("xdg/aic/config.toml");
    fs::write(&path, "[layers.l0\n")?;
    let output = command(dir.path())?
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .code(4)
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8(output)?.contains(&path.display().to_string()));
    Ok(())
}

#[test]
fn init_ignores_malformed_user_config() -> TestResult {
    let dir = fixture()?;
    fs::remove_dir(dir.path().join(".aic"))?;
    fs::write(dir.path().join("xdg/aic/config.toml"), "invalid = [")?;
    command(dir.path())?.arg("init").assert().success();
    let config = fs::read_to_string(dir.path().join(".aic/config.toml"))?;
    assert!(config.contains("cargo check"));
    Ok(())
}

#[test]
fn project_commands_replace_user_arrays_in_non_rust_repo() -> TestResult {
    let dir = fixture()?;
    fs::remove_file(dir.path().join("Cargo.toml"))?;
    let user = USER_CONFIG.replace("commands=[]", "commands=['exit 9']");
    fs::write(dir.path().join("xdg/aic/config.toml"), user)?;
    fs::write(
        dir.path().join(".aic/config.toml"),
        "[layers.l0]\ncommands=['cargo explicit']\n[layers.l1]\ncommands=[]\n",
    )?;
    command(dir.path())?
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(dir.path().join("calls"))?, "explicit\n");
    Ok(())
}

#[test]
fn invalid_user_setting_type_names_its_path() -> TestResult {
    let dir = fixture()?;
    let path = dir.path().join("xdg/aic/config.toml");
    fs::write(&path, "[layers.l0]\ncommands=7\n")?;
    let output = command(dir.path())?
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .code(4)
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8(output)?.contains(&path.display().to_string()));
    Ok(())
}

#[test]
fn relative_config_roots_are_ignored() -> TestResult {
    let dir = fixture()?;
    let malformed = "invalid = [";
    fs::create_dir_all(dir.path().join("rel/aic"))?;
    fs::write(dir.path().join("rel/aic/config.toml"), malformed)?;
    fs::write(dir.path().join("home/.config/aic/config.toml"), USER_CONFIG)?;
    command(dir.path())?
        .env("XDG_CONFIG_HOME", "rel")
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .success();
    fs::create_dir_all(dir.path().join(".config/aic"))?;
    fs::write(dir.path().join(".config/aic/config.toml"), malformed)?;
    fs::write(dir.path().join(".aic/config.toml"), USER_CONFIG)?;
    command(dir.path())?
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", "")
        .args(["audit", "--diff", "diff.patch"])
        .assert()
        .success();
    Ok(())
}
