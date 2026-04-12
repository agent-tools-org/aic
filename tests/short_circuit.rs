//! Early-exit integration test for blocking layer failures.
//! Exports: CLI short-circuit coverage; deps: assert_cmd, serde_json, tempfile.

use assert_cmd::Command;
use serde_json::Value;
use std::{error::Error, fs};
use tempfile::tempdir;

#[test]
fn fail_blocking_l0_prevents_later_layers_from_running() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    let aic_dir = dir.path().join(".aic");
    let rules_dir = aic_dir.join("rules");
    fs::create_dir_all(&rules_dir)?;
    fs::write(
        aic_dir.join("config.toml"),
        "[layers.l0]\ncommands = [\"false\"]\nsecret_scan = false\naddress_verify = false\n\n[layers.l1]\ncommands = [\"exit 0\"]\nmax_duration_secs = 5\n\n[layers.l2]\nenabled = true\nrules_dir = \".aic/rules\"\n\n[layers.l3]\nenabled = true\nmodels = [\"fake\"]\nswap = false\nconsensus_threshold = 1\n",
    )?;
    fs::write(
        rules_dir.join("check.md"),
        "---\nid: check-rule\nseverity: advisory\nscope: [\"src/**/*.rs\"]\nlayer: l2\n---\n\n# Check rule\n\nThis should never run.\n",
    )?;
    fs::write(
        dir.path().join("diff.patch"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+++ b/src/lib.rs\n@@\n+pub fn short_circuit() {}\n",
    )?;

    let output = Command::cargo_bin("aic")?
        .current_dir(dir.path())
        .args(["audit", "--diff", "diff.patch"])
        .output()?;

    assert_eq!(output.status.code(), Some(1));
    let result: Value = serde_json::from_str(&fs::read_to_string(
        dir.path().join(".aic").join("audits").join("diff").join("result.json"),
    )?)?;
    let layers = result["layer_results"].as_array().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing layer_results")
    })?;
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0]["layer"], "L0Static");
    assert!(layers[0]["verdict"].get("FailBlocking").is_some());
    assert!(!layers.iter().any(|layer| layer["layer"] == "L1Test"));
    assert!(!layers.iter().any(|layer| layer["layer"] == "L2Checklist"));
    assert!(!layers.iter().any(|layer| layer["layer"] == "L3CrossLlm"));
    Ok(())
}
