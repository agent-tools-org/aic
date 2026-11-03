//! Repository-sensitive default command and init regression tests.
//! Covers omitted keys, explicit overrides, and skipped L0/L1 evidence.
use aic::{
    config::Config,
    layers::{l0::L0Runner, l1::L1Runner},
};
use assert_cmd::Command;
use std::{error::Error, fs};
use tempfile::tempdir;

#[test]
fn defaults_follow_repository_and_preserve_explicit_commands() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("config.toml");
    let config = Config::load_for_repo(&path, dir.path())?;
    assert!(config.layers.l0.commands.is_empty());
    assert!(config.layers.l1.commands.is_empty());
    fs::write(
        &path,
        "[layers.l0]\nsecret_scan=false\naddress_verify=false\n[layers.l1]\n",
    )?;
    let config = Config::load_for_repo(&path, dir.path())?;
    let (_, evidence) = L0Runner::new(&config.layers.l0, dir.path().into()).run(&[]);
    assert!(evidence
        .iter()
        .any(|e| e.cite.as_deref() == Some("l0:skipped") && e.passed));
    let (_, evidence) = L1Runner::new(&config.layers.l1, dir.path().into()).run();
    assert_eq!(evidence[0].cite.as_deref(), Some("l1:skipped"));
    fs::write(
        &path,
        "[layers.l0]\ncommands=['exit 7']\n[layers.l1]\ncommands=[]\n",
    )?;
    let config = Config::load_for_repo(&path, dir.path())?;
    assert_eq!(config.layers.l0.commands, ["exit 7"]);
    assert!(config.layers.l1.commands.is_empty());
    fs::write(dir.path().join("Cargo.toml"), "[package]\n")?;
    let config = Config::load_for_repo(&path, dir.path())?;
    assert!(config.layers.l1.commands.is_empty());
    fs::write(
        &path,
        "[layers.l0]\ncommands=[]\n[layers.l1]\ncommands=['exit 9']\n",
    )?;
    let config = Config::load_for_repo(&path, dir.path())?;
    assert!(config.layers.l0.commands.is_empty());
    assert_eq!(config.layers.l1.commands, ["exit 9"]);
    fs::write(&path, "")?;
    let config = Config::load_for_repo(&path, dir.path())?;
    assert_eq!(
        config.layers.l0.commands,
        Config::default().layers.l0.commands
    );
    assert_eq!(
        config.layers.l1.commands,
        Config::default().layers.l1.commands
    );
    Ok(())
}

#[test]
fn init_writes_commands_for_each_repository_kind() -> Result<(), Box<dyn Error>> {
    for rust in [false, true] {
        let dir = tempdir()?;
        if rust {
            fs::write(dir.path().join("Cargo.toml"), "[package]\n")?;
        }
        Command::cargo_bin("aic")?
            .current_dir(dir.path())
            .arg("init")
            .assert()
            .success();
        let config = Config::load_for_repo(&dir.path().join(".aic/config.toml"), dir.path())?;
        assert_eq!(config, Config::for_repo(dir.path()));
        assert_eq!(config.layers.l2.model, "codex");
    }
    Ok(())
}
