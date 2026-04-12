// src/persist.rs
// Audit result persistence and Markdown rendering. Exports: persist_audit/report renderer.
// Deps: crate::types, serde_json, std::fs/path.

use crate::types::{AuditResult, Verdict};
use anyhow::{Context, Result};
use std::{fs, path::{Path, PathBuf}};

const MAX_EVIDENCE_DETAIL_CHARS: usize = 240;

pub fn persist_audit(task_id: &str, result: &AuditResult, repo_root: &Path) -> Result<PathBuf> {
    let audit_dir = repo_root.join(".aic").join("audits").join(task_id);
    fs::create_dir_all(&audit_dir).with_context(|| format!("create {}", audit_dir.display()))?;
    let json = serde_json::to_string_pretty(result).context("serialize audit result")?;
    fs::write(audit_dir.join("result.json"), json).with_context(|| format!("write result for {task_id}"))?;
    fs::write(audit_dir.join("report.md"), render_markdown_report(result))
        .with_context(|| format!("write report for {task_id}"))?;
    Ok(audit_dir)
}

pub fn render_markdown_report(result: &AuditResult) -> String {
    let mut report = String::new();
    report.push_str("# Audit Report\n\n");
    report.push_str(&format!("- Verdict: {}\n", verdict_label(&result.verdict)));
    report.push_str(&format!("- Duration: {} ms\n", result.duration_ms));
    report.push_str(&format!("- Cost: ${:.4}\n\n", result.total_cost_usd));
    report.push_str("## Layer Summary\n\n");
    report.push_str("| Layer | Verdict | Duration | Evidence |\n");
    report.push_str("| --- | --- | ---: | ---: |\n");
    for layer in &result.layer_results {
        report.push_str(&format!(
            "| {:?} | {} | {} ms | {} |\n",
            layer.layer,
            verdict_label(&layer.verdict),
            layer.duration_ms,
            layer.evidence_count,
        ));
    }
    report.push_str("\n## Evidence\n\n");
    if result.evidence.is_empty() {
        report.push_str("- No evidence recorded.\n");
        return report;
    }
    for entry in &result.evidence {
        report.push_str(&format!(
            "- {:?} {:?}: {}{}\n",
            entry.layer,
            entry.signal,
            truncate_detail(&entry.detail),
            cite_suffix(entry.cite.as_deref()),
        ));
    }
    report
}

fn verdict_label(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Pass => "Pass".to_owned(),
        Verdict::FailBlocking { reason } => format!("FailBlocking: {reason}"),
        Verdict::FailAdvisory { reason } => format!("FailAdvisory: {reason}"),
        Verdict::Inconclusive { reason } => format!("Inconclusive: {reason}"),
    }
}

fn cite_suffix(cite: Option<&str>) -> String {
    cite.map(|value| format!(" ({value})")).unwrap_or_default()
}

fn truncate_detail(detail: &str) -> String {
    let mut truncated = String::new();
    for ch in detail.chars().take(MAX_EVIDENCE_DETAIL_CHARS) {
        truncated.push(ch);
    }
    if detail.chars().count() > MAX_EVIDENCE_DETAIL_CHARS {
        truncated.push_str("...");
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AuditResult, Evidence, Layer, LayerResult, SignalKind};
    use tempfile::tempdir;

    fn synthetic_result() -> AuditResult {
        AuditResult {
            verdict: Verdict::Pass,
            layer_results: vec![LayerResult {
                layer: Layer::L0Static,
                verdict: Verdict::Pass,
                duration_ms: 7,
                evidence_count: 1,
            }],
            evidence: vec![Evidence {
                layer: Layer::L0Static,
                signal: SignalKind::Compile,
                passed: true,
                detail: "compile passed".to_owned(),
                cite: Some("cargo check".to_owned()),
                cost_usd: 0.0,
            }],
            total_cost_usd: 0.0,
            duration_ms: 9,
        }
    }

    #[test]
    fn persist_writes_json_and_report() -> Result<()> {
        let dir = tempdir()?;
        let audit_dir = persist_audit("task-1", &synthetic_result(), dir.path())?;

        assert_eq!(audit_dir, dir.path().join(".aic").join("audits").join("task-1"));
        assert!(audit_dir.join("result.json").is_file());
        assert!(audit_dir.join("report.md").is_file());
        Ok(())
    }

    #[test]
    fn markdown_report_contains_verdict_and_layers() {
        let report = render_markdown_report(&synthetic_result());

        assert!(report.contains("Verdict: Pass"));
        assert!(report.contains("## Layer Summary"));
        assert!(report.contains("L0Static"));
    }
}
