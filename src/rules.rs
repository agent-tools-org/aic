// src/rules.rs
// Rule markdown parsing and scope matching. Exports: Rule types and directory loader.
// Deps: anyhow, globset, serde_yaml, std::fs.

use anyhow::{anyhow, bail, Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub severity: RuleSeverity,
    pub scope: Vec<String>,
    pub layer: RuleLayer,
    pub title: String,
    pub body: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleSeverity { Blocking, Advisory }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleLayer { L2, L3 }

#[derive(Debug, Deserialize)]
struct Frontmatter {
    id: String,
    severity: RuleSeverity,
    scope: Vec<String>,
    layer: RuleLayer,
}

impl Rule {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        parse_rule(path, &text)
    }

    pub fn matches(&self, file_path: &Path) -> bool {
        compile_scope(&self.scope).map(|set| set.is_match(file_path)).unwrap_or(false)
    }
}

pub fn load_rules_dir(dir: &Path) -> Result<Vec<Rule>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("list {}", dir.display()))?;
    paths.retain(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"));
    paths.sort();
    paths.into_iter().map(|path| Rule::load_from_file(&path)).collect()
}

fn parse_rule(path: &Path, text: &str) -> Result<Rule> {
    let (frontmatter, body) = split_frontmatter(text)?;
    let meta: Frontmatter = serde_yaml::from_str(frontmatter)
        .with_context(|| format!("parse frontmatter in {}", path.display()))?;
    if meta.scope.is_empty() {
        bail!("rule {} has empty scope", path.display());
    }
    compile_scope(&meta.scope)?;
    let body = body.trim_start().to_owned();
    let title = first_h1(&body).ok_or_else(|| anyhow!("rule {} is missing an H1 title", path.display()))?;
    Ok(Rule {
        id: meta.id,
        severity: meta.severity,
        scope: meta.scope,
        layer: meta.layer,
        title,
        body,
        source_path: path.to_path_buf(),
    })
}

fn split_frontmatter(text: &str) -> Result<(&str, &str)> {
    let rest = text.strip_prefix("---\n").ok_or_else(|| anyhow!("rule file is missing frontmatter"))?;
    rest.split_once("\n---\n").ok_or_else(|| anyhow!("rule file has an unterminated frontmatter block"))
}

fn first_h1(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn compile_scope(scope: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in scope {
        builder.add(Glob::new(pattern).with_context(|| format!("invalid glob pattern {pattern}"))?);
    }
    builder.build().context("build scope glob set")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_rule() -> &'static str {
        "---\nid: no-fee-direction-swap\nseverity: blocking\nscope: [\"dex/**/*.rs\"]\nlayer: l2\n---\n\n# Fee direction must be preserved\n\nCheck every hop.\n"
    }

    #[test]
    fn parses_rule_from_string_literal() {
        let rule = parse_rule(Path::new("rule.md"), sample_rule()).expect("parse rule");
        assert_eq!(rule.id, "no-fee-direction-swap");
        assert_eq!(rule.severity, RuleSeverity::Blocking);
        assert_eq!(rule.scope, vec!["dex/**/*.rs"]);
        assert_eq!(rule.layer, RuleLayer::L2);
        assert_eq!(rule.title, "Fee direction must be preserved");
        assert!(rule.body.starts_with("# Fee direction must be preserved"));
    }

    #[test]
    fn loads_rules_directory() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("a.md"), sample_rule()).expect("write a");
        fs::write(dir.path().join("b.md"), sample_rule().replace("l2", "l3")).expect("write b");
        let rules = load_rules_dir(dir.path()).expect("load rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1].layer, RuleLayer::L3);
    }

    #[test]
    fn matches_scope_glob() {
        let rule = parse_rule(Path::new("rule.md"), sample_rule()).expect("parse rule");
        assert!(rule.matches(Path::new("dex/uniswap/v3.rs")));
        assert!(!rule.matches(Path::new("router/main.rs")));
    }

    #[test]
    fn rejects_missing_frontmatter() {
        let err = parse_rule(Path::new("rule.md"), "# Missing metadata\n").expect_err("missing frontmatter");
        assert!(err.to_string().contains("missing frontmatter"));
    }

    #[test]
    fn rejects_invalid_severity() {
        let text = sample_rule().replace("blocking", "critical");
        parse_rule(Path::new("rule.md"), &text).expect_err("invalid severity");
    }
}
