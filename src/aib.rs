//! ai-board subprocess client for audit verdict transitions.
//! Exports: AibClient and TransitionOutcome; deps: anyhow, std::process, crate::types::Verdict.

use crate::types::Verdict;
use anyhow::{anyhow, bail, Context, Result};
use std::{fs, path::Path, process::Command, sync::Arc};

const DEFAULT_BINARY: &str = "ai-board";
const NOOP_STATUS: &str = "noop";
pub struct AibClient {
    pub binary: String,
    runner: Arc<dyn CommandRunner>,
}
pub struct TransitionOutcome {
    pub from_status: Option<String>,
    pub to_status: String,
    pub attached: bool,
}
struct RunOutput { stdout: String }
trait CommandRunner: Send + Sync {
    fn run(&self, binary: &str, args: &[String]) -> Result<RunOutput>;
}
struct ProcessRunner;
impl CommandRunner for ProcessRunner {
    fn run(&self, binary: &str, args: &[String]) -> Result<RunOutput> {
        let output = Command::new(binary).args(args).output().with_context(|| format!("run {binary} {}", args.join(" ")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            bail!("{binary} {} failed: {}", args.join(" "), empty_stderr(&stderr));
        }
        Ok(RunOutput { stdout: String::from_utf8_lossy(&output.stdout).into_owned() })
    }
}
impl Default for AibClient {
    fn default() -> Self { Self::new() }
}

impl AibClient {
    pub fn new() -> Self { Self::with_binary(DEFAULT_BINARY) }
    pub fn with_binary(binary: impl Into<String>) -> Self { Self { binary: binary.into(), runner: Arc::new(ProcessRunner) } }
    pub fn apply_verdict(&self, item_id: &str, verdict: &Verdict, report_path: &Path) -> Result<TransitionOutcome> {
        let Some(to_status) = target_status(verdict) else {
            tracing::info!("aib: no-op for inconclusive verdict on {item_id}");
            return Ok(TransitionOutcome { from_status: None, to_status: NOOP_STATUS.to_owned(), attached: false });
        };
        let from_status = self.show_status(item_id);
        self.attach_report(item_id, &report_title(verdict), report_path)?;
        self.run(["update", item_id, "--status", to_status])?;
        Ok(TransitionOutcome { from_status, to_status: to_status.to_owned(), attached: true })
    }
    pub fn attach_report(&self, item_id: &str, title: &str, report_path: &Path) -> Result<()> {
        fs::read_to_string(report_path).with_context(|| format!("read {}", report_path.display()))?;
        let report = report_path.to_str().ok_or_else(|| anyhow!("non-utf8 report path: {}", report_path.display()))?;
        self.run(["item", "attach", item_id, "--artifact-type", "audit-report", "--title", title, "--path", report])?;
        Ok(())
    }
    #[cfg(test)]
    fn with_runner(binary: impl Into<String>, runner: Arc<dyn CommandRunner>) -> Self { Self { binary: binary.into(), runner } }
    fn run<I, S>(&self, args: I) -> Result<RunOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        self.runner.run(&self.binary, &args)
    }
    fn show_status(&self, item_id: &str) -> Option<String> {
        self.run(["show", item_id]).ok().and_then(|output| parse_status(&output.stdout))
    }
}

fn target_status(verdict: &Verdict) -> Option<&'static str> {
    match verdict {
        Verdict::Pass | Verdict::FailAdvisory { .. } => Some("review"),
        Verdict::FailBlocking { .. } => Some("blocked"),
        Verdict::Inconclusive { .. } => None,
    }
}

fn report_title(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Pass => "Audit Report: Pass",
        Verdict::FailBlocking { .. } => "Audit Report: FailBlocking",
        Verdict::FailAdvisory { .. } => "Audit Report: FailAdvisory",
        Verdict::Inconclusive { .. } => "Audit Report: Inconclusive",
    }
    .to_owned()
}

fn parse_status(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let lower = line.trim().to_ascii_lowercase();
        ["status:", "status ="].iter().find_map(|prefix| {
            lower.strip_prefix(prefix).map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned)
        })
    })
}

fn empty_stderr(stderr: &str) -> &str {
    if stderr.is_empty() { "no stderr" } else { stderr }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};
    use tempfile::NamedTempFile;

    struct MockRunner {
        calls: Mutex<Vec<Vec<String>>>,
        responses: Mutex<VecDeque<Result<RunOutput>>>,
    }

    impl MockRunner {
        fn new(responses: Vec<Result<RunOutput>>) -> Arc<Self> {
            Arc::new(Self { calls: Mutex::new(Vec::new()), responses: Mutex::new(VecDeque::from(responses)) })
        }
        fn calls(&self) -> Vec<Vec<String>> { self.calls.lock().expect("calls lock").clone() }
    }
    impl CommandRunner for MockRunner {
        fn run(&self, binary: &str, args: &[String]) -> Result<RunOutput> {
            self.calls.lock().expect("calls lock").push(std::iter::once(binary.to_owned()).chain(args.iter().cloned()).collect());
            self.responses.lock().expect("responses lock").pop_front().expect("mock response")
        }
    }
    fn ok(stdout: &str) -> Result<RunOutput> { Ok(RunOutput { stdout: stdout.to_owned() }) }
    fn report_file() -> NamedTempFile {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), "# report\n").expect("write report");
        file
    }
    #[test]
    fn apply_verdict_pass_updates_review_and_attaches() {
        let runner = MockRunner::new(vec![ok("status: todo"), ok(""), ok("")]);
        let client = AibClient::with_runner("ai-board", runner.clone());
        let report = report_file();
        let outcome = client.apply_verdict("wi-1", &Verdict::Pass, report.path()).expect("apply verdict");
        let calls = runner.calls();
        assert_eq!(outcome.from_status.as_deref(), Some("todo"));
        assert_eq!(outcome.to_status, "review");
        assert!(outcome.attached);
        assert_eq!(calls[1][..8], ["ai-board", "item", "attach", "wi-1", "--artifact-type", "audit-report", "--title", "Audit Report: Pass"]);
        assert_eq!(calls[1][8], "--path");
        assert_eq!(calls[1][9], report.path().to_str().expect("path"));
        assert_eq!(calls[2], vec!["ai-board", "update", "wi-1", "--status", "review"]);
    }
    #[test]
    fn apply_verdict_blocking_updates_blocked_and_attaches() {
        let runner = MockRunner::new(vec![ok("status: review"), ok(""), ok("")]);
        let client = AibClient::with_runner("ai-board", runner.clone());
        let report = report_file();
        let outcome = client.apply_verdict("wi-2", &Verdict::FailBlocking { reason: "stop".into() }, report.path()).expect("apply verdict");
        assert_eq!(outcome.to_status, "blocked");
        assert_eq!(runner.calls()[2], vec!["ai-board", "update", "wi-2", "--status", "blocked"]);
    }
    #[test]
    fn apply_verdict_advisory_updates_review() {
        let runner = MockRunner::new(vec![ok("status: todo"), ok(""), ok("")]);
        let client = AibClient::with_runner("ai-board", runner.clone());
        let report = report_file();
        let outcome = client.apply_verdict("wi-3", &Verdict::FailAdvisory { reason: "warn".into() }, report.path()).expect("apply verdict");
        assert_eq!(outcome.to_status, "review");
        assert_eq!(runner.calls()[2], vec!["ai-board", "update", "wi-3", "--status", "review"]);
    }
    #[test]
    fn apply_verdict_inconclusive_is_noop() {
        let runner = MockRunner::new(Vec::new());
        let client = AibClient::with_runner("ai-board", runner.clone());
        let outcome = client.apply_verdict("wi-4", &Verdict::Inconclusive { reason: "unknown".into() }, Path::new("report.md")).expect("apply verdict");
        assert_eq!(outcome.to_status, NOOP_STATUS);
        assert!(!outcome.attached);
        assert!(runner.calls().is_empty());
    }
    #[test]
    fn attach_report_passes_title_and_path() {
        let runner = MockRunner::new(vec![ok("")]);
        let client = AibClient::with_runner("ai-board", runner.clone());
        let report = report_file();
        client.attach_report("wi-5", "My Title", report.path()).expect("attach report");
        assert_eq!(runner.calls()[0], vec!["ai-board", "item", "attach", "wi-5", "--artifact-type", "audit-report", "--title", "My Title", "--path", report.path().to_str().expect("path")]);
    }
    #[test]
    fn subprocess_errors_surface_stderr() {
        let runner = MockRunner::new(vec![Err(anyhow!("stderr: boom"))]);
        let client = AibClient::with_runner("ai-board", runner);
        let report = report_file();
        let error = client.attach_report("wi-6", "Title", report.path()).expect_err("attach should fail");
        assert!(error.to_string().contains("stderr: boom"));
    }
}
