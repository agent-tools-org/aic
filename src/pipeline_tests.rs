//! Regression tests for pipeline.
//! Uses parent module helpers and isolated test fixtures.
use super::*;
use crate::config::{Integration, L0, L1, L2, L3, Layers};
use crate::types::{SignalKind, Verdict};
use tempfile::tempdir;

#[test]
fn parses_touched_files_from_unified_diff() -> Result<()> {
    let dir = tempdir()?;
    let diff = dir.path().join("diff.patch");
    fs::write(&diff, "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/main.rs\n+++ /dev/null\n")?;

    let files = crate::touched_paths::parse(&fs::read_to_string(&diff)?);

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
fn execute_empty_diff_returns_pass_without_running_llm_layers() -> Result<()> {
    let dir = tempdir()?;
    let diff = dir.path().join("diff.patch");
    fs::write(&diff, "")?;
    let config = Config {
        layers: Layers {
            l0: L0 { commands: vec!["exit 1".to_owned()], secret_scan: false, address_verify: false },
            l1: L1 { commands: vec!["exit 1".to_owned()], max_duration_secs: 5 },
            l2: L2 { enabled: true, ..L2::default() },
            l3: L3 { enabled: true, ..L3::default() },
            ..Layers::default()
        },
        integration: Integration::default(),
    };
    let request = AuditRequest {
        task_id: Some("empty".to_owned()),
        diff_path: diff,
        artifacts: Vec::new(),
        context: Default::default(),
        skip_layers: Vec::new(),
        budget_usd: None,
    };

    let result = Pipeline::new(&config, dir.path().to_path_buf()).execute(request)?;

    assert!(matches!(result.verdict, Verdict::Pass));
    assert_eq!(result.layer_results.len(), 1);
    assert_eq!(result.layer_results[0].layer, Layer::L0Static);
    assert_eq!(result.evidence.len(), 1);
    assert!(result.evidence[0].detail.contains("empty diff"));
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
