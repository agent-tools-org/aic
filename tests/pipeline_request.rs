//! Pipeline request field integration tests.
//! Exports: tests for skip_layers and budget_usd; deps: aic, tempfile.

use aic::config::{Config, Integration, L0, L1, L2, L3, Layers};
use aic::pipeline::Pipeline;
use aic::{AuditRequest, Evidence, Layer, LayerResult, Verdict};
use std::{error::Error, fs, path::{Path, PathBuf}};
#[cfg(unix)]
use std::sync::Mutex;
use tempfile::tempdir;

#[cfg(unix)]
static PATH_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn execute_skips_l3_without_layer_result_or_evidence() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    let config = pipeline_config(true, 2.0);
    let request = audit_request(write_diff(dir.path())?, vec![Layer::L3CrossLlm], None);

    let result = Pipeline::new(&config, dir.path().to_path_buf()).execute(request)?;

    assert!(!has_layer(&result.layer_results, Layer::L3CrossLlm));
    assert!(!has_evidence(&result.evidence, Layer::L3CrossLlm));
    Ok(())
}

#[test]
fn execute_with_all_layers_skipped_is_inconclusive_with_empty_results() -> Result<(), Box<dyn Error>> {
    let dir = tempdir()?;
    let config = pipeline_config(true, 2.0);
    let request = audit_request(
        write_diff(dir.path())?,
        vec![Layer::L0Static, Layer::L1Test, Layer::L2Checklist, Layer::L3CrossLlm],
        None,
    );

    let result = Pipeline::new(&config, dir.path().to_path_buf()).execute(request)?;

    assert!(result.layer_results.is_empty());
    assert!(result.evidence.is_empty());
    assert_eq!(result.verdict, Verdict::Inconclusive { reason: "No conclusive audit verdict".to_owned() });
    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_uses_request_budget_override() -> Result<(), Box<dyn Error>> {
    let _lock = PATH_LOCK.lock().expect("lock PATH");
    let dir = tempdir()?;
    let _path_guard = install_fake_aid(dir.path())?;
    let config = pipeline_config(true, 2.0);
    let request = audit_request(write_diff(dir.path())?, Vec::new(), Some(0.001));

    let result = Pipeline::new(&config, dir.path().to_path_buf()).execute(request)?;

    assert_eq!(result.total_cost_usd, 0.05);
    assert!(matches!(result.verdict, Verdict::Inconclusive { ref reason } if reason.contains("exceeds budget 0.0010")));
    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_defaults_to_config_budget() -> Result<(), Box<dyn Error>> {
    let _lock = PATH_LOCK.lock().expect("lock PATH");
    let dir = tempdir()?;
    let _path_guard = install_fake_aid(dir.path())?;
    let config = pipeline_config(true, 0.001);
    let request = audit_request(write_diff(dir.path())?, Vec::new(), None);

    let result = Pipeline::new(&config, dir.path().to_path_buf()).execute(request)?;

    assert_eq!(result.total_cost_usd, 0.05);
    assert!(matches!(result.verdict, Verdict::Inconclusive { ref reason } if reason.contains("exceeds budget 0.0010")));
    Ok(())
}

#[cfg(unix)]
#[test]
fn execute_passes_request_artifacts_to_l2_and_l3_prompts() -> Result<(), Box<dyn Error>> {
    let _lock = PATH_LOCK.lock().expect("lock PATH");
    let dir = tempdir()?;
    let log_path = dir.path().join("aid-prompts.log");
    let _path_guard = install_fake_aid_with_logging(dir.path(), &log_path)?;
    let rules_dir = dir.path().join(".aic").join("rules");
    fs::create_dir_all(&rules_dir)?;
    fs::write(
        rules_dir.join("l2.md"),
        "---\nid: l2-rule\nseverity: advisory\nscope: [\"src/**/*.rs\"]\nlayer: l2\n---\n\n# L2 rule\n\nApply context.\n",
    )?;
    fs::write(
        rules_dir.join("l3.md"),
        "---\nid: l3-rule\nseverity: advisory\nscope: [\"src/**/*.rs\"]\nlayer: l3\n---\n\n# L3 rule\n\nApply context.\n",
    )?;
    let artifact = dir.path().join("context.txt");
    fs::write(&artifact, "artifact <CONTEXT_FILE> body")?;
    let diff = write_diff(dir.path())?;
    let config = Config {
        layers: Layers {
            l0: L0 { commands: Vec::new(), secret_scan: false, address_verify: false },
            l1: L1 { commands: vec!["exit 0".to_owned()], max_duration_secs: 5 },
            l2: L2 { enabled: true, rules_dir: ".aic/rules".into(), ..L2::default() },
            l3: L3 { enabled: true, models: vec!["fake".into()], swap: false, consensus_threshold: 1 },
            ..Layers::default()
        },
        integration: Integration::default(),
    };
    let mut request = audit_request(diff, Vec::new(), None);
    request.artifacts = vec![artifact];

    let result = Pipeline::new(&config, dir.path().to_path_buf()).execute(request)?;
    let log = fs::read_to_string(log_path)?;

    assert!(matches!(result.verdict, Verdict::Pass));
    assert!(log.contains("<CONTEXT_FILE path='"));
    assert!(log.matches("artifact <CONTEXT-FILE> body").count() >= 2);
    Ok(())
}

fn write_diff(dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let diff = dir.join("diff.patch");
    fs::write(&diff, "diff --git a/src/lib.rs b/src/lib.rs\n+++ b/src/lib.rs\n")?;
    Ok(diff)
}

fn audit_request(diff_path: PathBuf, skip_layers: Vec<Layer>, budget_usd: Option<f64>) -> AuditRequest {
    AuditRequest { task_id: Some("unit".to_owned()), diff_path, artifacts: Vec::new(), context: Default::default(), skip_layers, budget_usd }
}

fn pipeline_config(l3_enabled: bool, budget_usd_per_audit: f64) -> Config {
    Config {
        layers: Layers {
            l0: L0 { commands: Vec::new(), secret_scan: false, address_verify: false },
            l1: L1 { commands: vec!["exit 0".to_owned()], max_duration_secs: 5 },
            l2: L2 { enabled: false, ..L2::default() },
            l3: L3 { enabled: l3_enabled, models: vec!["fake".into()], swap: false, consensus_threshold: 1 },
            ..Layers::default()
        },
        integration: Integration { budget_usd_per_audit, ..Integration::default() },
    }
}

fn has_layer(results: &[LayerResult], layer: Layer) -> bool {
    results.iter().any(|result| result.layer == layer)
}

fn has_evidence(evidence: &[Evidence], layer: Layer) -> bool {
    evidence.iter().any(|item| item.layer == layer)
}

#[cfg(unix)]
struct PathGuard {
    previous: String,
}

#[cfg(unix)]
impl Drop for PathGuard {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.previous);
    }
}

#[cfg(unix)]
fn install_fake_aid(dir: &Path) -> Result<PathGuard, Box<dyn Error>> {
    install_fake_aid_script(
        dir,
        "#!/bin/sh\nprintf '%s\\n' '{\"verdict\":\"pass\",\"bugs\":[],\"severity\":\"advisory\"}'\n",
    )
}

#[cfg(unix)]
fn install_fake_aid_with_logging(dir: &Path, log_path: &Path) -> Result<PathGuard, Box<dyn Error>> {
    install_fake_aid_script(
        dir,
        &format!(
            concat!(
                "#!/bin/sh\n",
                "cat \"$5\" >> \"{}\" || exit 1\n",
                "printf '\\n--PROMPT--\\n' >> \"{}\"\n",
                "printf '%s\\n' '{{\"verdict\":\"pass\",\"reason\":\"ok\",",
                "\"cited_lines\":[],\"bugs\":[],\"severity\":\"advisory\"}}'\n",
            ),
            log_path.display(),
            log_path.display()
        ),
    )
}

#[cfg(unix)]
fn install_fake_aid_script(dir: &Path, script: &str) -> Result<PathGuard, Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let bin_dir = dir.join("bin");
    let aid_path = bin_dir.join("aid");
    fs::create_dir_all(&bin_dir)?;
    fs::write(&aid_path, script)?;
    let mut permissions = fs::metadata(&aid_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&aid_path, permissions)?;
    let previous = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{previous}", bin_dir.display()));
    Ok(PathGuard { previous })
}
