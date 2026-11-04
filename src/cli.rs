//! Clap-based command line interface for aic.
//! Exports: run; deps: config loading, clap derive, tracing subscriber.

use crate::aib::AibClient;
use crate::config::Config;
use crate::init;
use crate::persist::{persist_audit, render_markdown_report};
use crate::pipeline::Pipeline;
use crate::types::{AuditContext, AuditRequest, Verdict};
use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::NamedTempFile;
use tracing_subscriber::EnvFilter;

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose)?;
    match cli.command {
        Commands::Init => init::run()?,
        Commands::Audit(args) => {
            run_audit(args, cli.config.as_deref())?;
        }
        command => println!("not yet implemented: {}", command.stub_name()),
    }
    Ok(())
}

fn run_audit(args: AuditArgs, explicit_config: Option<&Path>) -> Result<()> {
    let repo_root = std::env::current_dir().context("resolve current directory")?;
    let task_mode = args.task_id.is_some();
    let board_item = args.board_item.clone();
    let (task_id, diff_path, task_diff) = audit_target(args, &repo_root)?;
    let base = if explicit_config.is_none() {
        task_mode.then(|| crate::base_config::BaseConfig::load(&task_id, &repo_root)).transpose()?
    } else { None };
    let config = match &base {
        Some(base) => base.config.clone(),
        None => Config::load(explicit_config.unwrap_or(Path::new(".aic/config.toml")))?,
    };
    let request = AuditRequest {
        task_id: Some(task_id.clone()),
        diff_path,
        artifacts: Vec::new(),
        context: AuditContext::default(),
        skip_layers: Vec::new(),
        budget_usd: Some(config.integration.budget_usd_per_audit),
    };
    let result = Pipeline::new(&config, repo_root.clone()).execute(request)?;
    drop(task_diff);
    let audit_dir = persist_audit(&task_id, &result, &repo_root)?;
    println!("{}", render_markdown_report(&result));
    if config.integration.aib_auto_block {
        apply_aib_verdict(
            board_item.as_deref(),
            &result.verdict,
            &audit_dir.join("report.md"),
        );
    }
    exit_for_verdict(&result.verdict);
    Ok(())
}

fn apply_aib_verdict(board_item: Option<&str>, verdict: &Verdict, report_path: &Path) {
    let Some(item_id) = board_item else { return };
    match AibClient::new().apply_verdict(item_id, verdict, report_path) {
        Ok(outcome) if outcome.to_status != "noop" => println!("aib: transitioned {item_id} to {}", outcome.to_status),
        Ok(_) => {}
        Err(error) => eprintln!("aib: failed to transition {item_id}: {error}"),
    }
}

fn audit_target(
    args: AuditArgs,
    repo_root: &Path,
) -> Result<(String, PathBuf, Option<NamedTempFile>)> {
    if let Some(diff) = args.diff {
        let task_id = diff
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("diff")
            .to_owned();
        return Ok((task_id, diff, None));
    }
    let task_id = args
        .task_id
        .ok_or_else(|| anyhow!("missing task-id or --diff"))?;
    // The id names the report directory, so it must not be able to leave it.
    if !task_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
        bail!("invalid task id: {task_id}");
    }
    let diff_path = fetch_task_diff(&task_id, repo_root)?;
    Ok((task_id, diff_path.path().to_owned(), Some(diff_path)))
}

fn fetch_task_diff(task_id: &str, repo_root: &Path) -> Result<NamedTempFile> {
    let hint = "requires an aid release with `aid show --patch` support";
    let output = Command::new("aid")
        .args(["show", task_id, "--patch"])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run aid show {task_id} --patch; {hint}"))?;
    if !output.status.success() {
        bail!(
            "aid show {task_id} --patch failed: {}; {hint}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut file = NamedTempFile::new().context("create task patch temporary file")?;
    file.write_all(&output.stdout)
        .context("write task patch temporary file")?;
    Ok(file)
}

fn exit_for_verdict(verdict: &Verdict) {
    let code = match verdict {
        Verdict::Pass => 0,
        Verdict::FailBlocking { .. } => 1,
        Verdict::FailAdvisory { .. } => 2,
        Verdict::Inconclusive { .. } => 3,
    };
    if code != 0 {
        std::process::exit(code);
    }
}

fn init_tracing(verbose: bool) -> Result<()> {
    let default_filter = if verbose { "debug" } else { "warn" };
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    tracing_subscriber::fmt().with_env_filter(env_filter).with_writer(std::io::stderr).try_init()
        .map_err(|error| anyhow!("initialize tracing subscriber: {error}"))
}

#[derive(Debug, Parser)]
#[command(name = "aic", version, about = "Cross-audit aid agent outputs")]
struct Cli {
    /// Explicit config override; task audits otherwise load .aic/config.toml from the patch base.
    #[arg(long, value_name = "path")] config: Option<PathBuf>,
    #[arg(short, long)] verbose: bool,
    #[command(subcommand)] command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Init,
    Audit(AuditArgs),
    Report { #[arg(value_name = "task-id")] task_id: String },
    Checklist { #[command(subcommand)] command: ChecklistCommands },
}

impl Commands {
    fn stub_name(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Audit(args) => args.stub_name(),
            Self::Report { task_id } => {
                let _target = task_id.as_str();
                "report"
            }
            Self::Checklist { command } => command.stub_name(),
        }
    }
}

#[derive(Debug, Args)]
struct AuditArgs {
    #[arg(value_name = "task-id", required_unless_present = "diff", conflicts_with = "diff")]
    task_id: Option<String>,
    #[arg(long, value_name = "path", conflicts_with = "task_id")] diff: Option<PathBuf>,
    #[arg(long, value_name = "id")] board_item: Option<String>,
}

impl AuditArgs {
    fn stub_name(&self) -> &'static str {
        let _task_id = self.task_id.as_deref();
        let _diff = self.diff.as_deref();
        let _board_item = self.board_item.as_deref();
        "audit"
    }
}

#[derive(Debug, Subcommand)]
enum ChecklistCommands {
    List,
}

impl ChecklistCommands {
    fn stub_name(&self) -> &'static str { match self { Self::List => "checklist list" } }
}
