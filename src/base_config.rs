//! Task configuration pinned to aid's recorded start commit, independent of checkout changes.
//! Materializes config and rule blobs with sanitized git commands; owns their temporary lifetime.

use crate::config::Config;
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

const HINT: &str =
    "requires a newer aid with `aid show --start-commit` support and a readable base commit";

pub struct BaseConfig {
    pub config: Config,
    _snapshot: TempDir,
}

impl BaseConfig {
    pub fn load(task: &str, repo: &Path) -> Result<Self> {
        if !task
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("invalid task id: {task}");
        }
        Self::load_inner(task, repo).with_context(|| format!("load task audit base; {HINT}"))
    }

    fn load_inner(task: &str, repo: &Path) -> Result<Self> {
        let output = Command::new("aid")
            .args(["show", task, "--start-commit"])
            .current_dir(repo)
            .output()
            .context("run aid show --start-commit")?;
        if !output.status.success() {
            bail!(
                "aid show --start-commit failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let text = std::str::from_utf8(&output.stdout).context("start commit is not UTF-8")?;
        let base = text.strip_suffix('\n').unwrap_or(text);
        if !matches!(base.len(), 40 | 64) || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("aid show --start-commit returned an invalid full hex commit id");
        }
        git(repo, &["cat-file", "-e", &format!("{base}^{{commit}}")])?;
        let snapshot = tempfile::tempdir().context("create base config snapshot")?;
        materialize(repo, base, Path::new(".aic/config.toml"), snapshot.path())?;
        materialize(repo, base, Path::new("Cargo.toml"), snapshot.path())?;
        let mut config =
            Config::load_for_repo(&snapshot.path().join(".aic/config.toml"), snapshot.path())?;
        if !Path::new(&config.layers.l2.rules_dir).is_absolute() {
            let rules = relative_path(&config.layers.l2.rules_dir)?;
            materialize(repo, base, &rules, snapshot.path())?;
            config.layers.l2.rules_dir = snapshot.path().join(rules).to_string_lossy().into_owned();
        } else {
            reject_project_absolute_rules(snapshot.path())?;
        }
        Ok(Self {
            config,
            _snapshot: snapshot,
        })
    }
}

fn reject_project_absolute_rules(snapshot: &Path) -> Result<()> {
    let path = snapshot.join(".aic/config.toml");
    if !path.exists() {
        return Ok(());
    }
    let project: toml::Value = toml::from_str(&fs::read_to_string(path)?)?;
    if project
        .get("layers")
        .and_then(|v| v.get("l2"))
        .and_then(|v| v.get("rules_dir"))
        .is_some()
    {
        bail!("project rules_dir must be relative to the repository");
    }
    Ok(())
}

fn relative_path(value: &str) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => bail!("config path escapes repository: {value}"),
        }
    }
    if result.as_os_str().is_empty() {
        bail!("relative rules_dir must not normalize to the repository root: {value}");
    }
    Ok(result)
}

fn materialize(repo: &Path, base: &str, path: &Path, dest: &Path) -> Result<()> {
    let name = path.to_str().context("non UTF-8 config path")?;
    reject_symlink_ancestors(repo, base, path)?;
    let listing = git(repo, &["ls-tree", "-rz", base, "--", name])?;
    for entry in listing.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        let entry = std::str::from_utf8(entry).context("non UTF-8 base tree path")?;
        let (metadata, file) = entry.split_once('\t').context("invalid git tree entry")?;
        let file_path = Path::new(file);
        if file_path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            bail!("base tree path contains a non-normal component: {file}");
        }
        if file_path != path && !file_path.starts_with(path) && !path.starts_with(file_path) {
            continue;
        }
        if !metadata.starts_with("100") {
            bail!("base config path is not a regular file: {file} (requested {name})");
        }
        let target = dest.join(file_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let oid = metadata
            .split_whitespace()
            .nth(2)
            .context("missing git blob id")?;
        fs::write(target, git(repo, &["cat-file", "blob", oid])?)?;
    }
    Ok(())
}

fn reject_symlink_ancestors(repo: &Path, base: &str, path: &Path) -> Result<()> {
    for ancestor in path
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
    {
        let name = ancestor.to_str().context("non UTF-8 config ancestor")?;
        let entry = git(repo, &["ls-tree", "-z", base, "--", name])?;
        if entry.starts_with(b"120000 ") || entry.starts_with(b"160000 ") {
            bail!("base config path traverses a non-directory: {name}");
        }
    }
    Ok(())
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command.env("GIT_NO_REPLACE_OBJECTS", "1");
    let output = command
        .current_dir(repo)
        .args(args)
        .output()
        .context("run base git command")?;
    if !output.status.success() {
        bail!(
            "base git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}
