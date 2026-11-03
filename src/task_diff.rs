//! Collect task changes from aid metadata and the task checkout's Git state.
//! Exports: fetch_task_diff; deps: anyhow, serde, serde_json, std filesystem/process.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[derive(Deserialize)]
struct TaskMetadata {
    start_sha: Option<String>,
    final_head_sha: Option<String>,
    worktree_path: Option<PathBuf>,
}

pub(crate) fn fetch_task_diff(task_id: &str, repo_root: &Path) -> Result<PathBuf> {
    let output = Command::new("aid")
        .args(["show", task_id, "--json"])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run aid show {task_id} --json"))?;
    if !output.status.success() {
        bail!(
            "aid show {task_id} --json failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let task: TaskMetadata =
        serde_json::from_slice(&output.stdout).with_context(|| format!("parse aid show {task_id} --json"))?;
    let start = task
        .start_sha
        .as_deref()
        .context("task metadata is missing start_sha")?;
    let checkout = task.worktree_path.as_deref().unwrap_or(repo_root);
    if !checkout.is_dir() {
        bail!("task checkout does not exist: {}", checkout.display());
    }
    let diff = collect_diff(checkout, start, task.final_head_sha.as_deref())?;
    let path = std::env::temp_dir().join(format!("aic-{task_id}-{}.diff", std::process::id()));
    fs::write(&path, diff).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

fn collect_diff(checkout: &Path, start: &str, final_head: Option<&str>) -> Result<Vec<u8>> {
    let mut head_command = git(checkout);
    head_command.args(["rev-parse", "--verify", "HEAD"]);
    let head = checked_output(head_command, false)?;
    let head = std::str::from_utf8(&head.stdout)
        .context("decode git HEAD")?
        .trim();
    let working_tree = match final_head {
        Some(final_head) => final_head == head,
        None => true,
    };
    let mut diff_command = git(checkout);
    diff_command.args(["diff", "--no-ext-diff", "--no-color", "--no-textconv", start]);
    if !working_tree {
        diff_command.arg(final_head.context("missing final_head_sha")?);
    }
    diff_command.arg("--");
    let mut diff = checked_output(diff_command, false)?.stdout;
    if working_tree {
        append_untracked(checkout, &mut diff)?;
    }
    Ok(diff)
}

fn append_untracked(checkout: &Path, diff: &mut Vec<u8>) -> Result<()> {
    let mut list_command = git(checkout);
    // aic's own reports land in .aic/ and are not part of the task's change.
    list_command.args(["ls-files", "--others", "--exclude-standard", "-z", "--", ".", ":(exclude).aic"]);
    let files = checked_output(list_command, false)?;
    for path in files
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let mut diff_command = git(checkout);
        diff_command.args([
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-textconv",
            "--no-index",
            "--",
        ]);
        diff_command.arg("/dev/null").arg(untracked_path(path)?);
        diff.extend(checked_output(diff_command, true)?.stdout);
    }
    Ok(())
}

fn git(checkout: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(checkout);
    command
}

fn checked_output(mut command: Command, allow_difference: bool) -> Result<Output> {
    let output = command.output().with_context(|| format!("run {command:?}"))?;
    if !output.status.success() && !(allow_difference && output.status.code() == Some(1)) {
        bail!(
            "{command:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

fn untracked_path(bytes: &[u8]) -> Result<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        Ok(OsString::from(
            std::str::from_utf8(bytes).context("decode untracked path")?,
        ))
    }
}
