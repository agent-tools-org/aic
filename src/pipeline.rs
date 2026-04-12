// src/pipeline.rs
// Audit pipeline orchestration. Exports: Pipeline and diff parsing helper.
// Deps: aggregator, config, layers, types, std::fs/time/path.

use crate::aggregator::Aggregator;
use crate::config::Config;
use crate::layers::{aid_dispatch::RealAidDispatcher, l0::L0Runner, l1::L1Runner, l2, l2::L2Runner, l3, l3::L3Runner};
use crate::rules::{load_rules_dir, Rule, RuleLayer};
use crate::types::{AuditRequest, AuditResult, Evidence, Layer, LayerResult};
use anyhow::{Context, Result};
use std::{fs, path::{Path, PathBuf}, time::Instant};

pub struct Pipeline<'cfg> {
    pub config: &'cfg Config,
    pub repo_root: PathBuf,
}

impl<'cfg> Pipeline<'cfg> {
    pub fn new(config: &'cfg Config, repo_root: PathBuf) -> Self {
        Self { config, repo_root }
    }

    pub fn execute(&self, request: AuditRequest) -> Result<AuditResult> {
        let start = Instant::now();
        let touched_files = parse_diff_touched_files(&request.diff_path)?;
        let diff_text = fs::read_to_string(&request.diff_path)
            .with_context(|| format!("read diff {}", request.diff_path.display()))?;
        let mut layer_results = Vec::new();
        let mut evidence = Vec::new();
        let budget = request.budget_usd.or(Some(self.config.integration.budget_usd_per_audit));
        let aid_dispatcher = RealAidDispatcher;

        if !should_skip(&request, Layer::L0Static) {
            let (l0_result, l0_evidence) = L0Runner::new(&self.config.layers.l0, self.repo_root.clone()).run(&touched_files);
            layer_results.push(l0_result);
            evidence.extend(l0_evidence);
            if has_blocking(&layer_results) {
                return Ok(self.finalize(layer_results, evidence, start, budget));
            }
            if over_budget(&evidence, budget) {
                return Ok(self.finalize(layer_results, evidence, start, budget));
            }
        }

        if !should_skip(&request, Layer::L1Test) {
            let (l1_result, l1_evidence) = L1Runner::new(&self.config.layers.l1, self.repo_root.clone()).run();
            layer_results.push(l1_result);
            evidence.extend(l1_evidence);
            if has_blocking(&layer_results) {
                return Ok(self.finalize(layer_results, evidence, start, budget));
            }
            if over_budget(&evidence, budget) {
                return Ok(self.finalize(layer_results, evidence, start, budget));
            }
        }

        if self.config.layers.l2.enabled && !should_skip(&request, Layer::L2Checklist) {
            let rules_dir = resolve_path(&self.repo_root, &self.config.layers.l2.rules_dir);
            let (l2_result, l2_evidence) =
                L2Runner::new(&self.config.layers.l2, rules_dir).run_with(&diff_text, &touched_files, &request.artifacts, &aid_dispatcher);
            layer_results.push(l2_result);
            evidence.extend(l2_evidence);
            if has_blocking(&layer_results) {
                return Ok(self.finalize(layer_results, evidence, start, budget));
            }
            if over_budget(&evidence, budget) {
                return Ok(self.finalize(layer_results, evidence, start, budget));
            }
        }

        if self.config.layers.l3.enabled && !should_skip(&request, Layer::L3CrossLlm) {
            let l3_rules = self.load_l3_rules(&touched_files)?;
            let matched_rules = l3_rules.iter().collect::<Vec<_>>();
            let (l3_result, l3_evidence) =
                L3Runner::new(&self.config.layers.l3).run_with(&diff_text, request.context.spec.as_deref(), &request.artifacts, &matched_rules, &aid_dispatcher);
            layer_results.push(l3_result);
            evidence.extend(l3_evidence);
            if over_budget(&evidence, budget) {
                return Ok(self.finalize(layer_results, evidence, start, budget));
            }
        }

        Ok(self.finalize(layer_results, evidence, start, budget))
    }

    fn finalize(&self, layer_results: Vec<LayerResult>, evidence: Vec<Evidence>, start: Instant, budget: Option<f64>) -> AuditResult {
        Aggregator::new(budget)
            .finalize(layer_results, evidence, start.elapsed())
    }

    fn load_l3_rules(&self, touched_files: &[PathBuf]) -> Result<Vec<Rule>> {
        let rules_dir = resolve_path(&self.repo_root, &self.config.layers.l2.rules_dir);
        Ok(load_rules_dir(&rules_dir)?
            .into_iter()
            .filter(|rule| rule.layer == RuleLayer::L3)
            .filter(|rule| touched_files.iter().any(|path| rule.matches(path)))
            .collect())
    }
}

fn should_skip(request: &AuditRequest, layer: Layer) -> bool {
    request.skip_layers.contains(&layer)
}

pub fn parse_diff_touched_files(path: &Path) -> Result<Vec<PathBuf>> {
    let text = fs::read_to_string(path).with_context(|| format!("read diff {}", path.display()))?;
    let mut files = Vec::new();
    for line in text.lines() {
        let candidate = if let Some(rest) = line.strip_prefix("diff --git ") {
            rest.split_whitespace().nth(1).and_then(|item| item.strip_prefix("b/"))
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            rest.split_whitespace().next().and_then(|item| item.strip_prefix("b/"))
        } else {
            None
        };
        if let Some(file) = candidate.filter(|file| *file != "/dev/null") {
            let path = PathBuf::from(file);
            if !files.contains(&path) {
                files.push(path);
            }
        }
    }
    Ok(files)
}

#[cfg_attr(not(test), allow(dead_code))]
fn layer_cost_estimate(result: &LayerResult) -> f64 {
    match result.layer {
        Layer::L2Checklist => result.evidence_count as f64 * l2::MODEL_COST_USD,
        Layer::L3CrossLlm => result.evidence_count as f64 * l3::MODEL_COST_USD,
        _ => 0.0,
    }
}

fn has_blocking(layer_results: &[LayerResult]) -> bool {
    layer_results.last().is_some_and(|result| result.verdict.is_blocking())
}

fn over_budget(evidence: &[Evidence], budget: Option<f64>) -> bool {
    budget.is_some_and(|limit| evidence.iter().map(|item| item.cost_usd).sum::<f64>() > limit)
}

fn resolve_path(repo_root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() { path } else { repo_root.join(path) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Integration, L0, L1, L2, L3, Layers};
    use crate::types::{SignalKind, Verdict};
    use tempfile::tempdir;

    #[test]
    fn parses_touched_files_from_unified_diff() -> Result<()> {
        let dir = tempdir()?;
        let diff = dir.path().join("diff.patch");
        fs::write(&diff, "diff --git a/src/lib.rs b/src/lib.rs\n+++ b/src/main.rs\n+++ /dev/null\n")?;

        let files = parse_diff_touched_files(&diff)?;

        assert_eq!(files, vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")]);
        Ok(())
    }

    #[test]
    fn execute_passes_with_trivial_enabled_layers() -> Result<()> {
        let dir = tempdir()?;
        let diff = dir.path().join("diff.patch");
        fs::write(&diff, "diff --git a/src/lib.rs b/src/lib.rs\n+++ b/src/lib.rs\n")?;
        let config = Config {
            layers: Layers {
                l0: L0 { commands: Vec::new(), secret_scan: false, address_verify: false },
                l1: L1 { commands: vec!["exit 0".to_owned()], max_duration_secs: 5 },
                l2: L2 { enabled: false, ..L2::default() },
                l3: L3 { enabled: false, ..L3::default() },
                ..Layers::default()
            },
            integration: Integration::default(),
        };
        let request = AuditRequest {
            task_id: Some("unit".to_owned()),
            diff_path: diff,
            artifacts: Vec::new(),
            context: Default::default(),
            skip_layers: Vec::new(),
            budget_usd: Some(config.integration.budget_usd_per_audit),
        };

        let result = Pipeline::new(&config, dir.path().to_path_buf()).execute(request)?;

        assert!(matches!(result.verdict, Verdict::Pass));
        Ok(())
    }

    #[test]
    fn load_l3_rules_filters_shared_rules_by_layer_and_scope() -> Result<()> {
        let dir = tempdir()?;
        let rules_dir = dir.path().join(".aic/rules");
        fs::create_dir_all(&rules_dir)?;
        fs::write(
            rules_dir.join("l3.md"),
            "---\nid: l3-rule\nseverity: blocking\nscope: [\"src/**/*.rs\"]\nlayer: l3\n---\n\n# L3 rule\n\nReview the diff.\n",
        )?;
        fs::write(
            rules_dir.join("l2.md"),
            "---\nid: l2-rule\nseverity: advisory\nscope: [\"src/**/*.rs\"]\nlayer: l2\n---\n\n# L2 rule\n\nIgnore for L3.\n",
        )?;
        let config = Config {
            layers: Layers {
                l0: L0 { commands: Vec::new(), secret_scan: false, address_verify: false },
                l1: L1 { commands: vec!["exit 0".to_owned()], max_duration_secs: 5 },
                l2: L2 { enabled: true, rules_dir: ".aic/rules".into(), ..L2::default() },
                l3: L3 { enabled: true, ..L3::default() },
                ..Layers::default()
            },
            integration: Integration::default(),
        };

        let rules = Pipeline::new(&config, dir.path().to_path_buf()).load_l3_rules(&[PathBuf::from("src/lib.rs")])?;

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "l3-rule");
        Ok(())
    }

    #[test]
    fn over_budget_detects_excess_running_cost() {
        let evidence = vec![Evidence {
            layer: Layer::L3CrossLlm,
            signal: SignalKind::LlmVote,
            passed: true,
            detail: "detail".to_owned(),
            cite: None,
            cost_usd: 0.05,
        }];

        assert!(over_budget(&evidence, Some(0.01)));
    }

    #[test]
    fn over_budget_is_false_without_budget_limit() {
        let evidence = vec![Evidence {
            layer: Layer::L3CrossLlm,
            signal: SignalKind::LlmVote,
            passed: true,
            detail: "detail".to_owned(),
            cite: None,
            cost_usd: 0.05,
        }];

        assert!(!over_budget(&evidence, None));
    }

    #[test]
    fn layer_cost_estimate_counts_l2_and_l3_evidence() {
        let l2_result = LayerResult {
            layer: Layer::L2Checklist,
            verdict: Verdict::Pass,
            duration_ms: 1,
            evidence_count: 2,
        };
        let l3_result = LayerResult {
            layer: Layer::L3CrossLlm,
            verdict: Verdict::Pass,
            duration_ms: 1,
            evidence_count: 3,
        };

        assert!((layer_cost_estimate(&l2_result) - 0.02).abs() < f64::EPSILON);
        assert!((layer_cost_estimate(&l3_result) - 0.15).abs() < f64::EPSILON);
    }
}
