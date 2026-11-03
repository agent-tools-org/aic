// src/pipeline.rs
// Audit pipeline orchestration. Exports: Pipeline; touched paths come from touched_paths.
// Deps: aggregator, config, layers, types, std::fs/time/path.

use crate::aggregator::Aggregator;
use crate::config::Config;
use crate::layers::{aid_dispatch::RealAidDispatcher, l0::L0Runner, l1::L1Runner, l2, l2::L2Runner, l3, l3::L3Runner};
use crate::rules::{load_rules_dir, Rule, RuleLayer};
use crate::types::{AuditRequest, AuditResult, Evidence, Layer, LayerResult, Verdict};
use anyhow::{Context, Result};
use std::{fs, path::{Path, PathBuf}, time::Instant};

/// A judge reads the diff through its own file tool, so a partial read must stay improbable.
const MAX_JUDGE_DIFF_BYTES: usize = 128 * 1024;

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
        let diff_bytes = fs::read(&request.diff_path)
            .with_context(|| format!("read diff {}", request.diff_path.display()))?;
        let diff_text = String::from_utf8_lossy(&diff_bytes);
        let touched_files = crate::touched_paths::parse(&diff_text);
        let mut layer_results = Vec::new();
        let mut evidence = Vec::new();

        // Empty-diff early exit: nothing to audit. Skip all LLM layers and return Pass.
        // Triggered when aid --audit wraps a pure text/research task with no code change.
        if diff_text.trim().is_empty() {
            let budget = request.budget_usd.or(Some(self.config.integration.budget_usd_per_audit));
            layer_results.push(LayerResult {
                layer: Layer::L0Static,
                verdict: crate::types::Verdict::Pass,
                duration_ms: 0,
                evidence_count: 1,
            });
            evidence.push(Evidence {
                layer: Layer::L0Static,
                signal: crate::types::SignalKind::Script,
                passed: true,
                detail: "empty diff — no code to audit, skipping LLM layers".into(),
                cite: Some("aic:empty-diff".into()),
                cost_usd: 0.0,
            });
            return Ok(self.finalize(layer_results, evidence, start, budget));
        }
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

        let judge = [Layer::L2Checklist, Layer::L3CrossLlm].into_iter().find(|layer| {
            !should_skip(&request, *layer) && match layer {
                Layer::L2Checklist => self.config.layers.l2.enabled,
                _ => self.config.layers.l3.enabled,
            }
        });
        if let Some(layer) = judge.filter(|_| diff_text.len() > MAX_JUDGE_DIFF_BYTES) {
            let reason = format!("diff of {} bytes exceeds the {MAX_JUDGE_DIFF_BYTES} byte judge limit", diff_text.len());
            layer_results.push(LayerResult { layer, verdict: Verdict::Inconclusive { reason: reason.clone() }, duration_ms: 0, evidence_count: 1 });
            evidence.push(Evidence { layer, signal: crate::types::SignalKind::Script, passed: false, detail: reason, cite: Some("aic:oversized-diff".into()), cost_usd: 0.0 });
            return Ok(self.finalize(layer_results, evidence, start, budget));
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
#[path = "pipeline_tests.rs"]
mod tests;
