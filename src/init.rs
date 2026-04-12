//! Initialize the on-disk `.aic/` scaffold for audits.
//! Exports: run; deps: config defaults, rule parser, std::fs.

use crate::{config::Config, rules::Rule};
use anyhow::{anyhow, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const CONFIG_COMMENT: &str = "# aic audit config — see docs/architecture.md §6\n\n";
const NO_UNWRAP_RULE: &str = concat!(
    "---\n",
    "id: no-unwrap-in-prod\n",
    "severity: advisory\n",
    "scope: ['src/**/*.rs']\n",
    "layer: l2\n",
    "---\n",
    "# No unwrap() in production code\n\n",
    "Flag uses of .",
    "unwrap() outside test modules (#[cfg(test)] or tests/). Prefer .expect() with a clear message, or ? with anyhow/Result propagation.\n\n",
    "Ask the model: Does the diff add any .",
    "unwrap() calls outside of test code? List each occurrence with file:line and suggest the preferred alternative.\n\n",
    "Pass if no new unwraps in non-test code. Fail otherwise.\n",
);
const LEGACY_STUB_NOTE: &str = "compat: replaces previous not yet implemented: init stub";
const NEXT_STEP: &str = "Run: aic audit --diff <path>.patch";

pub fn run() -> Result<()> {
    let repo_root = std::env::current_dir().context("resolve current directory")?;
    match run_in(&repo_root)? {
        InitOutcome::AlreadyExists => {
            println!("aic: .aic/ already exists, nothing to do");
            println!("{LEGACY_STUB_NOTE}");
        }
        InitOutcome::Created(paths) => print_success(&paths),
    }
    Ok(())
}

fn run_in(repo_root: &Path) -> Result<InitOutcome> {
    let paths = InitPaths::new(repo_root);
    if paths.aic_dir.exists() {
        return handle_existing_root(&paths.aic_dir);
    }
    create_directories(&paths)?;
    write_default_config(&paths.config_path)?;
    write_no_unwrap_rule(&paths.rule_path)?;
    fs::write(&paths.gitkeep_path, "").with_context(|| format!("write {}", paths.gitkeep_path.display()))?;
    Ok(InitOutcome::Created(paths))
}

fn handle_existing_root(aic_dir: &Path) -> Result<InitOutcome> {
    if aic_dir.is_dir() {
        return Ok(InitOutcome::AlreadyExists);
    }
    Err(anyhow!("{} exists and is not a directory", aic_dir.display()))
}

fn create_directories(paths: &InitPaths) -> Result<()> {
    for dir in [&paths.aic_dir, &paths.rules_dir, &paths.audits_dir, &paths.checks_dir] {
        fs::create_dir(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    Ok(())
}

fn write_default_config(path: &Path) -> Result<()> {
    let body = toml::to_string_pretty(&Config::default()).context("serialize default config")?;
    fs::write(path, format!("{CONFIG_COMMENT}{body}")).with_context(|| format!("write {}", path.display()))
}

fn write_no_unwrap_rule(path: &Path) -> Result<()> {
    fs::write(path, NO_UNWRAP_RULE).with_context(|| format!("write {}", path.display()))?;
    Rule::load_from_file(path)?;
    Ok(())
}

fn print_success(paths: &InitPaths) {
    println!("{LEGACY_STUB_NOTE}");
    for created in paths.created_items() {
        println!("created: {created}");
    }
    println!("{NEXT_STEP}");
}

#[derive(Debug)]
enum InitOutcome {
    AlreadyExists,
    Created(InitPaths),
}

#[derive(Debug)]
struct InitPaths {
    aic_dir: PathBuf,
    rules_dir: PathBuf,
    audits_dir: PathBuf,
    checks_dir: PathBuf,
    config_path: PathBuf,
    rule_path: PathBuf,
    gitkeep_path: PathBuf,
}

impl InitPaths {
    fn new(repo_root: &Path) -> Self {
        let aic_dir = repo_root.join(".aic");
        let rules_dir = aic_dir.join("rules");
        let audits_dir = aic_dir.join("audits");
        let checks_dir = aic_dir.join("checks");
        Self {
            config_path: aic_dir.join("config.toml"),
            rule_path: rules_dir.join("no-unwrap-in-prod.md"),
            gitkeep_path: checks_dir.join(".gitkeep"),
            aic_dir,
            rules_dir,
            audits_dir,
            checks_dir,
        }
    }

    fn created_items(&self) -> [&'static str; 7] {
        [
            ".aic/",
            ".aic/rules/",
            ".aic/audits/",
            ".aic/checks/",
            ".aic/config.toml",
            ".aic/rules/no-unwrap-in-prod.md",
            ".aic/checks/.gitkeep",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        panic::{self, AssertUnwindSafe},
        sync::{Mutex, OnceLock},
    };
    use tempfile::tempdir;

    static CURRENT_DIR_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn run_creates_expected_scaffold() {
        with_temp_current_dir(|root| {
            run().expect("init scaffold");
            assert!(root.join(".aic").is_dir());
            assert!(root.join(".aic/rules").is_dir());
            assert!(root.join(".aic/audits").is_dir());
            assert!(root.join(".aic/checks").is_dir());
            assert!(root.join(".aic/config.toml").is_file());
            assert!(root.join(".aic/rules/no-unwrap-in-prod.md").is_file());
            assert!(root.join(".aic/checks/.gitkeep").is_file());
        });
    }

    #[test]
    fn run_is_idempotent_when_aic_dir_exists() {
        with_temp_current_dir(|root| {
            fs::create_dir(root.join(".aic")).expect("create .aic");
            run().expect("idempotent init");
            assert!(root.join(".aic").is_dir());
            assert!(!root.join(".aic/config.toml").exists());
            assert!(!root.join(".aic/rules").exists());
        });
    }

    #[test]
    fn generated_config_round_trips_to_default() {
        let dir = tempdir().expect("tempdir");
        let repo_root = dir.path();
        run_in(repo_root).expect("init scaffold");
        let config = Config::load(&repo_root.join(".aic/config.toml")).expect("load config");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn generated_rule_parses_successfully() {
        let dir = tempdir().expect("tempdir");
        let repo_root = dir.path();
        run_in(repo_root).expect("init scaffold");
        let rule = Rule::load_from_file(&repo_root.join(".aic/rules/no-unwrap-in-prod.md")).expect("load rule");
        assert_eq!(rule.id, "no-unwrap-in-prod");
        assert_eq!(rule.title, "No unwrap() in production code");
    }

    #[test]
    fn no_unwrap_rule_matches_src_scope() {
        let dir = tempdir().expect("tempdir");
        let repo_root = dir.path();
        run_in(repo_root).expect("init scaffold");
        let rule = Rule::load_from_file(&repo_root.join(".aic/rules/no-unwrap-in-prod.md")).expect("load rule");
        assert!(rule.matches(Path::new("src/foo.rs")));
    }

    fn with_temp_current_dir(test: impl FnOnce(&Path)) {
        let _lock = CURRENT_DIR_LOCK.get_or_init(|| Mutex::new(())).lock().expect("lock");
        let dir = tempdir().expect("tempdir");
        let original = env::current_dir().expect("current dir");
        env::set_current_dir(dir.path()).expect("switch current dir");
        let result = panic::catch_unwind(AssertUnwindSafe(|| test(dir.path())));
        env::set_current_dir(original).expect("restore current dir");
        if let Err(payload) = result {
            panic::resume_unwind(payload);
        }
    }
}
