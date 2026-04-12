// src/aggregator.rs
// Verdict aggregation. Exports: Aggregator; deps: crate::types, std::time.

use crate::types::{AuditResult, Evidence, LayerResult, Verdict};
use std::time::Duration;

const DEFAULT_INCONCLUSIVE_REASON: &str = "No conclusive audit verdict";

pub struct Aggregator {
    pub budget_usd: Option<f64>,
}

impl Aggregator {
    pub fn new(budget_usd: Option<f64>) -> Self {
        Self { budget_usd }
    }

    pub fn finalize(
        &self,
        layer_results: Vec<LayerResult>,
        evidence: Vec<Evidence>,
        duration: Duration,
    ) -> AuditResult {
        let total_cost_usd = total_cost(&evidence);
        let verdict = if let Some(reason) = reasons_for(&layer_results, is_blocking) {
            Verdict::FailBlocking { reason }
        } else if self.is_over_budget(total_cost_usd) {
            Verdict::Inconclusive { reason: budget_reason(total_cost_usd, self.budget_usd) }
        } else if let Some(reason) = reasons_for(&layer_results, is_advisory) {
            Verdict::FailAdvisory { reason }
        } else if all_passed(&layer_results) {
            Verdict::Pass
        } else {
            Verdict::Inconclusive { reason: DEFAULT_INCONCLUSIVE_REASON.to_owned() }
        };

        AuditResult {
            verdict,
            layer_results,
            evidence,
            total_cost_usd,
            duration_ms: duration.as_millis() as u64,
        }
    }

    fn is_over_budget(&self, total_cost_usd: f64) -> bool {
        self.budget_usd.is_some_and(|budget| total_cost_usd > budget)
    }
}

fn total_cost(evidence: &[Evidence]) -> f64 {
    evidence.iter().map(|entry| entry.cost_usd).sum()
}

fn budget_reason(total_cost_usd: f64, budget_usd: Option<f64>) -> String {
    match budget_usd {
        Some(budget_usd) => format!("Evidence cost {total_cost_usd:.4} exceeds budget {budget_usd:.4}"),
        None => DEFAULT_INCONCLUSIVE_REASON.to_owned(),
    }
}

fn reasons_for<F>(layer_results: &[LayerResult], predicate: F) -> Option<String>
where
    F: Fn(&Verdict) -> Option<&str>,
{
    let reasons: Vec<&str> = layer_results
        .iter()
        .filter_map(|result| predicate(&result.verdict))
        .collect();
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

fn is_blocking(verdict: &Verdict) -> Option<&str> {
    match verdict {
        Verdict::FailBlocking { reason } => Some(reason),
        _ => None,
    }
}

fn is_advisory(verdict: &Verdict) -> Option<&str> {
    match verdict {
        Verdict::FailAdvisory { reason } => Some(reason),
        _ => None,
    }
}

fn all_passed(layer_results: &[LayerResult]) -> bool {
    !layer_results.is_empty() && layer_results.iter().all(|result| matches!(result.verdict, Verdict::Pass))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Layer, SignalKind};

    fn layer_result(layer: Layer, verdict: Verdict) -> LayerResult {
        LayerResult {
            layer,
            verdict,
            duration_ms: 10,
            evidence_count: 1,
        }
    }

    fn evidence(cost_usd: f64) -> Evidence {
        Evidence {
            layer: Layer::L4Evidence,
            signal: SignalKind::LlmVote,
            passed: true,
            detail: "detail".to_owned(),
            cite: None,
            cost_usd,
        }
    }

    #[test]
    fn all_pass_results_pass() {
        let result = Aggregator::new(None).finalize(
            vec![
                layer_result(Layer::L0Static, Verdict::Pass),
                layer_result(Layer::L1Test, Verdict::Pass),
            ],
            vec![evidence(0.1)],
            Duration::from_millis(25),
        );

        assert!(matches!(result.verdict, Verdict::Pass));
        assert_eq!(result.duration_ms, 25);
    }

    #[test]
    fn one_fail_blocking_results_fail_blocking() {
        let result = Aggregator::new(None).finalize(
            vec![
                layer_result(Layer::L0Static, Verdict::Pass),
                layer_result(Layer::L1Test, Verdict::FailBlocking { reason: "tests failed".to_owned() }),
            ],
            Vec::new(),
            Duration::from_millis(1),
        );

        assert_eq!(result.verdict, Verdict::FailBlocking { reason: "tests failed".to_owned() });
    }

    #[test]
    fn only_fail_advisory_results_fail_advisory() {
        let result = Aggregator::new(None).finalize(
            vec![layer_result(
                Layer::L0Static,
                Verdict::FailAdvisory { reason: "secret scanner unavailable".to_owned() },
            )],
            Vec::new(),
            Duration::from_millis(1),
        );

        assert_eq!(
            result.verdict,
            Verdict::FailAdvisory { reason: "secret scanner unavailable".to_owned() }
        );
    }

    #[test]
    fn over_budget_without_fails_results_inconclusive() {
        let result = Aggregator::new(Some(0.5)).finalize(
            vec![layer_result(Layer::L0Static, Verdict::Pass)],
            vec![evidence(0.25), evidence(0.30)],
            Duration::from_millis(1),
        );

        assert!(matches!(result.verdict, Verdict::Inconclusive { .. }));
    }

    #[test]
    fn empty_layer_results_results_inconclusive() {
        let result = Aggregator::new(None).finalize(Vec::new(), Vec::new(), Duration::from_millis(1));

        assert_eq!(
            result.verdict,
            Verdict::Inconclusive { reason: DEFAULT_INCONCLUSIVE_REASON.to_owned() }
        );
    }

    #[test]
    fn fail_blocking_wins_over_pass_and_fail_advisory() {
        let result = Aggregator::new(None).finalize(
            vec![
                layer_result(Layer::L0Static, Verdict::Pass),
                layer_result(Layer::L1Test, Verdict::FailBlocking { reason: "compile failed".to_owned() }),
                layer_result(Layer::L2Checklist, Verdict::FailAdvisory { reason: "docs missing".to_owned() }),
            ],
            Vec::new(),
            Duration::from_millis(1),
        );

        assert_eq!(result.verdict, Verdict::FailBlocking { reason: "compile failed".to_owned() });
    }

    #[test]
    fn total_cost_usd_matches_evidence_cost_sum() {
        let result = Aggregator::new(None).finalize(
            vec![layer_result(Layer::L0Static, Verdict::Pass)],
            vec![evidence(0.25), evidence(0.50), evidence(0.125)],
            Duration::from_millis(1),
        );

        assert_eq!(result.total_cost_usd, 0.875);
    }
}
