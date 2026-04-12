// src/layers/l1.rs
// L1 test checks. Exports: L1Runner; deps: crate::config, crate::types, std::process, wait_timeout.

use crate::config::L1;
use crate::types::{Evidence, Layer, LayerResult, SignalKind, Verdict};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

const MAX_DETAIL_BYTES: usize = 8192;

pub struct L1Runner<'cfg> {
    pub config: &'cfg L1,
    pub repo_root: PathBuf,
}

impl<'cfg> L1Runner<'cfg> {
    pub fn new(config: &'cfg L1, repo_root: PathBuf) -> Self { Self { config, repo_root } }

    pub fn run(&self) -> (LayerResult, Vec<Evidence>) {
        if self.config.commands.is_empty() {
            let evidence = vec![Evidence {
                layer: Layer::L1Test,
                signal: SignalKind::Test,
                passed: true,
                detail: "no L1 commands configured — layer skipped".into(),
                cite: Some("l1:skipped".into()),
                cost_usd: 0.0,
            }];
            return (
                LayerResult { layer: Layer::L1Test, verdict: Verdict::Pass, duration_ms: 0, evidence_count: 1 },
                evidence,
            );
        }
        let started_at = Instant::now();
        let evidence = self.collect_evidence();
        let result = LayerResult {
            layer: Layer::L1Test,
            verdict: verdict_from_evidence(&evidence),
            duration_ms: started_at.elapsed().as_millis() as u64,
            evidence_count: evidence.len(),
        };
        (result, evidence)
    }

    fn collect_evidence(&self) -> Vec<Evidence> {
        self.config.commands.iter().map(|command| self.run_command(command)).collect()
    }

    fn run_command(&self, command: &str) -> Evidence {
        match spawn_shell(command, &self.repo_root) {
            Ok(mut child) => run_child(command, self.config.max_duration_secs, &mut child),
            Err(error) => failure_evidence(command, error.to_string())
        }
    }
}

fn verdict_from_evidence(evidence: &[Evidence]) -> Verdict {
    if evidence.is_empty() {
        return Verdict::Inconclusive { reason: "L1 has no commands configured".to_owned() };
    }
    let failed: Vec<&str> = evidence
        .iter()
        .filter(|entry| !entry.passed)
        .filter_map(|entry| entry.cite.as_deref())
        .collect();
    if !failed.is_empty() {
        return Verdict::FailBlocking { reason: format!("L1 command failed: {}", failed.join(", ")) };
    }
    Verdict::Pass
}

fn spawn_shell(command: &str, cwd: &Path) -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        let mut shell = Command::new("sh");
        shell
            .arg("-c")
            .arg(command)
            .process_group(0)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        shell.spawn()
    }
    #[cfg(windows)]
    {
        // TODO: Use Windows Job Objects so timeout cleanup can kill descendants too.
        Command::new("cmd")
            .args(["/C", command])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    }
}

fn run_child(command: &str, timeout_secs: u64, child: &mut Child) -> Evidence {
    let stdout = spawn_reader(child.stdout.take());
    let stderr = spawn_reader(child.stderr.take());
    match child.wait_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Some(status)) => finished_evidence(command, status, stdout, stderr),
        Ok(None) => timeout_evidence(command, timeout_secs, child, stdout, stderr),
        Err(error) => failure_with_output(command, error.to_string(), stdout, stderr),
    }
}

fn finished_evidence(
    command: &str,
    status: ExitStatus,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
) -> Evidence {
    let detail = truncate_detail(combined_output(stdout, stderr));
    let passed = status.code() == Some(0);
    Evidence {
        layer: Layer::L1Test,
        signal: SignalKind::Test,
        passed,
        detail,
        cite: Some(command.to_owned()),
        cost_usd: 0.0,
    }
}

fn timeout_evidence(
    command: &str,
    timeout_secs: u64,
    child: &mut Child,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
) -> Evidence {
    kill_process_group(child);
    let _ = child.wait();
    let output = combined_output(stdout, stderr);
    let detail = if output.is_empty() {
        format!("TIMEOUT after {timeout_secs}s")
    } else {
        format!("TIMEOUT after {timeout_secs}s\n{output}")
    };
    Evidence {
        layer: Layer::L1Test,
        signal: SignalKind::Test,
        passed: false,
        detail: truncate_detail(detail),
        cite: Some(command.to_owned()),
        cost_usd: 0.0,
    }
}

fn kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: kill delegates to the OS; the pgid is derived from a live child pid.
        let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    }
    #[cfg(windows)]
    {
        let _ = child.kill();
    }
}

fn failure_with_output(
    command: &str,
    error: String,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
) -> Evidence {
    let output = combined_output(stdout, stderr);
    let detail = if output.is_empty() { error } else { format!("{error}\n{output}") };
    failure_evidence(command, detail)
}

fn failure_evidence(command: &str, detail: String) -> Evidence {
    Evidence {
        layer: Layer::L1Test,
        signal: SignalKind::Test,
        passed: false,
        detail: truncate_detail(detail),
        cite: Some(command.to_owned()),
        cost_usd: 0.0,
    }
}

fn spawn_reader<T>(reader: Option<T>) -> JoinHandle<Vec<u8>>
where
    T: Read + Send + 'static,
{
    thread::spawn(move || read_stream(reader))
}

fn read_stream<T>(reader: Option<T>) -> Vec<u8>
where
    T: Read,
{
    let mut bytes = Vec::new();
    if let Some(mut reader) = reader {
        let _ = reader.read_to_end(&mut bytes);
    }
    bytes
}

fn combined_output(stdout: JoinHandle<Vec<u8>>, stderr: JoinHandle<Vec<u8>>) -> String {
    let stdout = join_bytes(stdout);
    let stderr = join_bytes(stderr);
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => String::from_utf8_lossy(&stdout).into_owned(),
        (true, false) => String::from_utf8_lossy(&stderr).into_owned(),
        (false, false) => {
            let stdout = String::from_utf8_lossy(&stdout);
            let stderr = String::from_utf8_lossy(&stderr);
            format!("{stdout}\n{stderr}")
        }
    }
}

fn join_bytes(handle: JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn collect_timeout_evidence(command: &str) -> (Evidence, Duration) {
        let config = L1 { commands: vec![command.into()], max_duration_secs: 1 };
        let runner = L1Runner::new(&config, PathBuf::from("."));
        let started_at = Instant::now();
        let evidence = runner.collect_evidence().pop().expect("timeout command should emit evidence");
        (evidence, started_at.elapsed())
    }

    #[test]
    fn true_command_passes() {
        let config = L1 { commands: vec!["true".into()], max_duration_secs: 1 };
        let runner = L1Runner::new(&config, PathBuf::from("."));
        let evidence = runner.collect_evidence();
        let (result, run_evidence) = runner.run();
        assert_eq!(evidence.len(), 1); assert!(evidence[0].passed);
        assert!(matches!(result.verdict, Verdict::Pass));
        assert_eq!(run_evidence.len(), 1);
    }

    #[test]
    fn false_command_blocks() {
        let config = L1 { commands: vec!["false".into()], max_duration_secs: 1 };
        let runner = L1Runner::new(&config, PathBuf::from("."));
        let evidence = runner.collect_evidence();
        let (result, run_evidence) = runner.run();
        assert_eq!(evidence.len(), 1); assert!(!evidence[0].passed);
        assert!(matches!(result.verdict, Verdict::FailBlocking { .. }));
        assert_eq!(run_evidence.len(), 1);
    }

    #[test]
    fn long_command_times_out() {
        let (evidence, elapsed) = collect_timeout_evidence("sleep 2");
        assert!(!evidence.passed); assert!(evidence.detail.starts_with("TIMEOUT after 1s"));
        assert!(elapsed < Duration::from_secs(3));
        assert!(matches!(verdict_from_evidence(std::slice::from_ref(&evidence)), Verdict::FailBlocking { .. }));
    }

    #[test]
    fn background_child_is_killed_on_timeout() {
        let (evidence, elapsed) = collect_timeout_evidence("sleep 10 & wait");
        assert!(!evidence.passed); assert!(evidence.detail.starts_with("TIMEOUT after 1s"));
        assert!(elapsed < Duration::from_secs(3));
    }

    #[test]
    fn empty_commands_are_skipped_with_pass() {
        let config = L1 { commands: Vec::new(), max_duration_secs: 1 };
        let runner = L1Runner::new(&config, PathBuf::from("."));
        let (result, run_evidence) = runner.run();
        assert!(matches!(result.verdict, Verdict::Pass));
        assert_eq!(result.duration_ms, 0); assert_eq!(result.evidence_count, 1);
        assert_eq!(run_evidence.len(), 1); assert!(run_evidence[0].passed);
        assert_eq!(run_evidence[0].detail, "no L1 commands configured — layer skipped");
        assert_eq!(run_evidence[0].cite.as_deref(), Some("l1:skipped"));
    }
}
