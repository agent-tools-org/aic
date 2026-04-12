// src/layers/l0.rs
// L0 static checks. Exports: L0Runner; deps: crate::config, crate::types, std::process.

use crate::config::L0;
use crate::types::{Evidence, Layer, LayerResult, SignalKind, Verdict};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

const MAX_DETAIL_BYTES: usize = 4096;
const GITLEAKS_SCAN: &str = "gitleaks dir .";
const ADDRESS_VERIFY_RELATIVE: &str = ".claude/tools/verify-addresses.sh";

pub struct L0Runner<'cfg> {
    pub config: &'cfg L0,
    pub repo_root: PathBuf,
}

impl<'cfg> L0Runner<'cfg> {
    pub fn new(config: &'cfg L0, repo_root: PathBuf) -> Self {
        Self { config, repo_root }
    }

    pub fn run(&self, touched_files: &[PathBuf]) -> (LayerResult, Vec<Evidence>) {
        let started_at = Instant::now();
        let evidence = self.collect_evidence(touched_files);
        let result = LayerResult {
            layer: Layer::L0Static,
            verdict: verdict_from_evidence(&evidence),
            duration_ms: started_at.elapsed().as_millis() as u64,
            evidence_count: evidence.len(),
        };
        (result, evidence)
    }

    fn collect_evidence(&self, touched_files: &[PathBuf]) -> Vec<Evidence> {
        let mut evidence = Vec::new();
        for command in &self.config.commands {
            evidence.push(self.run_command(command, SignalKind::Compile, command.to_owned()));
        }
        if self.config.secret_scan && command_available("gitleaks") {
            evidence.push(self.run_command(GITLEAKS_SCAN, SignalKind::Script, "gitleaks".to_owned()));
        }
        if self.config.address_verify {
            if let Some(script) = address_verify_path() {
                if touches_addressable_sources(touched_files) && script.is_file() {
                    let cite = script.display().to_string();
                    evidence.push(self.run_path_command(&script, SignalKind::Script, cite));
                }
            }
        }
        evidence
    }

    fn run_command(&self, command: &str, signal: SignalKind, cite: String) -> Evidence {
        let output = shell_output(command, &self.repo_root);
        evidence_from_output(signal, cite, output)
    }

    fn run_path_command(&self, path: &Path, signal: SignalKind, cite: String) -> Evidence {
        let output = Command::new(path).current_dir(&self.repo_root).output();
        evidence_from_output(signal, cite, output)
    }
}

fn verdict_from_evidence(evidence: &[Evidence]) -> Verdict {
    let blocking: Vec<&str> = evidence
        .iter()
        .filter(|entry| entry.signal == SignalKind::Compile && !entry.passed)
        .filter_map(|entry| entry.cite.as_deref())
        .collect();
    if !blocking.is_empty() {
        return Verdict::FailBlocking { reason: format!("L0 command failed: {}", blocking.join(", ")) };
    }

    let advisory: Vec<&str> = evidence
        .iter()
        .filter(|entry| entry.signal == SignalKind::Script && !entry.passed)
        .filter_map(|entry| entry.cite.as_deref())
        .collect();
    if !advisory.is_empty() {
        return Verdict::FailAdvisory { reason: format!("L0 advisory check failed: {}", advisory.join(", ")) };
    }

    Verdict::Pass
}

fn shell_output(command: &str, cwd: &Path) -> std::io::Result<Output> {
    #[cfg(unix)]
    {
        Command::new("sh").arg("-c").arg(command).current_dir(cwd).output()
    }
    #[cfg(windows)]
    {
        Command::new("cmd").args(["/C", command]).current_dir(cwd).output()
    }
}

fn evidence_from_output(
    signal: SignalKind,
    cite: String,
    output: std::io::Result<Output>,
) -> Evidence {
    match output {
        Ok(output) => Evidence {
            layer: Layer::L0Static,
            signal,
            passed: output.status.success(),
            detail: truncate_detail(combined_output(&output)),
            cite: Some(cite),
            cost_usd: 0.0,
        },
        Err(error) => Evidence {
            layer: Layer::L0Static,
            signal,
            passed: false,
            detail: truncate_detail(error.to_string()),
            cite: Some(cite),
            cost_usd: 0.0,
        },
    }
}

fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.into_owned(),
        (true, false) => stderr.into_owned(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

fn truncate_detail(detail: String) -> String {
    if detail.len() <= MAX_DETAIL_BYTES {
        return detail;
    }
    let mut truncated = String::new();
    for ch in detail.chars() {
        if truncated.len() + ch.len_utf8() > MAX_DETAIL_BYTES {
            break;
        }
        truncated.push(ch);
    }
    truncated
}

fn command_available(binary: &str) -> bool {
    Command::new(binary).arg("--version").output().is_ok()
}

fn address_verify_path() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| PathBuf::from(home).join(ADDRESS_VERIFY_RELATIVE))
}

fn touches_addressable_sources(files: &[PathBuf]) -> bool {
    files.iter().any(|path| matches!(path.extension().and_then(|ext| ext.to_str()), Some("rs" | "ts" | "sol")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn run_captures_evidence_and_blocks_on_failed_command() {
        let config = L0 {
            commands: vec!["true".into(), "false".into()],
            secret_scan: false,
            address_verify: false,
        };
        let runner = L0Runner::new(&config, PathBuf::from("."));
        let evidence = runner.collect_evidence(&[]);

        assert_eq!(evidence.len(), 2);
        assert!(evidence[0].passed);
        assert!(!evidence[1].passed);
        assert_eq!(evidence[0].signal, SignalKind::Compile);
        assert_eq!(evidence[1].cite.as_deref(), Some("false"));

        let (result, run_evidence) = runner.run(&[]);
        assert!(matches!(result.verdict, Verdict::FailBlocking { .. }));
        assert_eq!(result.evidence_count, 2);
        assert_eq!(run_evidence.len(), 2);
    }

    #[test]
    fn run_passes_when_commands_succeed() {
        let config = L0 {
            commands: vec!["echo ok".into()],
            secret_scan: false,
            address_verify: false,
        };
        let runner = L0Runner::new(&config, PathBuf::from("."));
        let evidence = runner.collect_evidence(&[]);

        assert_eq!(evidence.len(), 1);
        assert!(evidence[0].passed);
        assert!(evidence[0].detail.contains("ok"));

        let (result, run_evidence) = runner.run(&[]);
        assert!(matches!(result.verdict, Verdict::Pass));
        assert_eq!(result.evidence_count, 1);
        assert_eq!(run_evidence.len(), 1);
    }
}
