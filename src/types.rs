// src/types.rs
// Audit domain types. Exports: AuditRequest/Result/Evidence/Verdict/Layer/SignalKind.
// Deps: serde, std::path.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type TaskId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRequest {
    pub task_id: Option<TaskId>,
    pub diff_path: PathBuf,
    pub artifacts: Vec<PathBuf>,
    pub context: AuditContext,
    pub skip_layers: Vec<Layer>,
    pub budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditContext {
    pub spec: Option<String>,
    pub related_files: Vec<PathBuf>,
    pub claims: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Layer { L0Static, L1Test, L2Checklist, L3CrossLlm, L4Evidence }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditResult {
    pub verdict: Verdict,
    pub layer_results: Vec<LayerResult>,
    pub evidence: Vec<Evidence>,
    pub total_cost_usd: f64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Pass,
    FailBlocking { reason: String },
    FailAdvisory { reason: String },
    Inconclusive { reason: String },
}

impl Verdict {
    pub fn is_pass(&self) -> bool { matches!(self, Self::Pass) }

    pub fn is_blocking(&self) -> bool { matches!(self, Self::FailBlocking { .. }) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerResult {
    pub layer: Layer,
    pub verdict: Verdict,
    pub duration_ms: u64,
    pub evidence_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub layer: Layer,
    pub signal: SignalKind,
    pub passed: bool,
    pub detail: String,
    pub cite: Option<String>,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalKind { Compile, Test, Checklist, LlmVote, Crpc, Script }

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct EnumWrap<T> {
        value: T,
    }

    fn round_trip<T>(value: T)
    where
        T: Clone + std::fmt::Debug + PartialEq + Serialize + for<'de> Deserialize<'de>,
    {
        let encoded = toml::to_string(&EnumWrap { value: value.clone() }).expect("serialize enum");
        let decoded: EnumWrap<T> = toml::from_str(&encoded).expect("deserialize enum");
        assert_eq!(decoded.value, value);
    }

    #[test]
    fn layer_variants_round_trip() {
        for value in [Layer::L0Static, Layer::L1Test, Layer::L2Checklist, Layer::L3CrossLlm, Layer::L4Evidence] {
            round_trip(value);
        }
    }

    #[test]
    fn verdict_variants_round_trip() {
        for value in [
            Verdict::Pass,
            Verdict::FailBlocking { reason: "blocking".into() },
            Verdict::FailAdvisory { reason: "advisory".into() },
            Verdict::Inconclusive { reason: "unknown".into() },
        ] {
            round_trip(value);
        }
    }

    #[test]
    fn signal_kind_variants_round_trip() {
        for value in [SignalKind::Compile, SignalKind::Test, SignalKind::Checklist, SignalKind::LlmVote, SignalKind::Crpc, SignalKind::Script] {
            round_trip(value);
        }
    }

    #[test]
    fn verdict_helpers_match_truth_table() {
        assert!(Verdict::Pass.is_pass());
        assert!(!Verdict::Pass.is_blocking());
        assert!(!Verdict::FailAdvisory { reason: "warn".into() }.is_pass());
        assert!(!Verdict::FailAdvisory { reason: "warn".into() }.is_blocking());
        assert!(!Verdict::Inconclusive { reason: "unknown".into() }.is_pass());
        assert!(!Verdict::Inconclusive { reason: "unknown".into() }.is_blocking());
        assert!(!Verdict::FailBlocking { reason: "stop".into() }.is_pass());
        assert!(Verdict::FailBlocking { reason: "stop".into() }.is_blocking());
    }
}
