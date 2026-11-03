//! TOML configuration loader for aic.
//! Exports: Config and layer settings; deps: serde, toml, anyhow.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::ErrorKind, path::Path};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    #[serde(default)] pub layers: Layers,
    #[serde(default)] pub integration: Integration,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_for_repo(path, &std::env::current_dir().context("resolve repository root")?)
    }

    pub fn for_repo(repo_root: &Path) -> Self {
        let mut config = Self::default();
        if !repo_root.join("Cargo.toml").is_file() {
            config.layers.l0.commands.clear();
            config.layers.l1.commands.clear();
        }
        config
    }

    pub fn load_for_repo(path: &Path, repo_root: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::for_repo(repo_root)),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let value: toml::Value = toml::from_str(&contents)
            .with_context(|| format!("parse {}", path.display()))?;
        let has_commands = |layer: &str| {
            value.get("layers").and_then(|layers| layers.get(layer))
                .and_then(|settings| settings.get("commands")).is_some()
        };
        let l0_explicit = has_commands("l0");
        let l1_explicit = has_commands("l1");
        let mut config: Self = value.try_into()
            .with_context(|| format!("parse {}", path.display()))?;
        if !repo_root.join("Cargo.toml").is_file() {
            if !l0_explicit {
                config.layers.l0.commands.clear();
            }
            if !l1_explicit {
                config.layers.l1.commands.clear();
            }
        }
        Ok(config)
    }
}
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Layers {
    #[serde(default)] pub l0: L0,
    #[serde(default)] pub l1: L1,
    #[serde(default)] pub l2: L2,
    #[serde(default)] pub l3: L3,
    #[serde(default)] pub l4: L4,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct L0 {
    #[serde(default = "l0_commands")] pub commands: Vec<String>,
    #[serde(default = "default_true")] pub secret_scan: bool,
    #[serde(default = "default_true")] pub address_verify: bool,
}
impl Default for L0 {
    fn default() -> Self { Self { commands: l0_commands(), secret_scan: true, address_verify: true } }
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct L1 {
    #[serde(default = "l1_commands")] pub commands: Vec<String>,
    #[serde(default = "l1_max_duration_secs")] pub max_duration_secs: u64,
}
impl Default for L1 {
    fn default() -> Self { Self { commands: l1_commands(), max_duration_secs: l1_max_duration_secs() } }
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct L2 {
    #[serde(default = "default_true")] pub enabled: bool,
    #[serde(default = "l2_model")] pub model: String,
    #[serde(default = "rules_dir")] pub rules_dir: String,
}
impl Default for L2 {
    fn default() -> Self { Self { enabled: true, model: l2_model(), rules_dir: rules_dir() } }
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct L3 {
    #[serde(default = "default_true")] pub enabled: bool,
    /// v0.1 dogfood found small-model judges (flash-lite, haiku) do not reliably follow strict-JSON output schemas. Default to the capable model only; users who want multi-model consensus can add them back and raise the threshold.
    #[serde(default = "l3_models")] pub models: Vec<String>,
    #[serde(default = "default_true")] pub swap: bool,
    #[serde(default = "consensus_threshold")] pub consensus_threshold: u8,
}
impl Default for L3 {
    fn default() -> Self {
        Self { enabled: true, models: l3_models(), swap: true, consensus_threshold: consensus_threshold() }
    }
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct L4 {
    #[serde(default = "l4_scripts")] pub scripts: Vec<String>,
}
impl Default for L4 {
    fn default() -> Self { Self { scripts: l4_scripts() } }
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Integration {
    #[serde(default)] pub aid_auto_audit: bool,
    #[serde(default = "default_true")] pub aib_auto_block: bool,
    /// Default raised in v0.2 after dogfood run found real L3 runs cost $1.00-$2.50 per audit.
    #[serde(default = "budget_usd_per_audit")] pub budget_usd_per_audit: f64,
}
impl Default for Integration {
    fn default() -> Self { Self { aid_auto_audit: false, aib_auto_block: true, budget_usd_per_audit: budget_usd_per_audit() } }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
fn default_true() -> bool { true }
fn l0_commands() -> Vec<String> { strings(&["cargo check", "cargo clippy -- -D warnings"]) }
fn l1_commands() -> Vec<String> { strings(&["cargo test --lib"]) }
fn l1_max_duration_secs() -> u64 { 120 }
fn l2_model() -> String { "codex".to_owned() }
fn rules_dir() -> String { ".aic/rules".to_owned() }
fn l3_models() -> Vec<String> { strings(&["codex"]) }
fn consensus_threshold() -> u8 { 1 }
fn l4_scripts() -> Vec<String> { strings(&[".aic/checks/*.sh"]) }
fn budget_usd_per_audit() -> f64 { 5.0 }
