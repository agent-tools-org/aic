// src/layers/aid_dispatch.rs
// Shared `aid` CLI dispatch. Exports: AidResponse, AidDispatcher, RealAidDispatcher, parse helpers; deps: anyhow, json_extract, std::process, std::time.

use crate::layers::json_extract::extract_json_object;
use anyhow::{anyhow, bail, Context, Result};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct AidResponse {
    pub task_id: String,
    pub stdout: String,
    pub cost_usd: f64,
    pub duration_ms: u64,
}

pub trait AidDispatcher {
    fn dispatch(&self, agent: &str, prompt: &str) -> Result<AidResponse>;
}

pub struct RealAidDispatcher;

impl AidDispatcher for RealAidDispatcher {
    fn dispatch(&self, agent: &str, prompt: &str) -> Result<AidResponse> {
        let output = Command::new("aid")
            .arg("run")
            .arg(agent)
            .arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn `aid run {agent}`"))?
            .wait_with_output()
            .with_context(|| format!("failed waiting for `aid run {agent}`"))?;
        let run_output = combined_output(&output.stdout, &output.stderr);
        if !output.status.success() {
            bail!("`aid run {agent}` failed: {}", format_command_output(&run_output));
        }
        if parse_task_id(&run_output).is_none() && extract_json_object(&run_output).is_some() {
            return Ok(AidResponse { task_id: String::new(), stdout: run_output, cost_usd: 0.0, duration_ms: 0 });
        }
        let task_id = parse_task_id(&run_output)
            .ok_or_else(|| anyhow!("failed to parse aid task id from output: {}", format_command_output(&run_output)))?;
        let (duration, cost_usd) = parse_done_line(&run_output).unwrap_or((Duration::default(), 0.0));
        let show_output = Command::new("aid")
            .arg("show")
            .arg(&task_id)
            .arg("--output")
            .output()
            .with_context(|| format!("failed to run `aid show {task_id} --output`"))?;
        if !show_output.status.success() {
            let show_output_text = combined_output(&show_output.stdout, &show_output.stderr);
            bail!("`aid show {task_id} --output` failed: {}", format_command_output(&show_output_text));
        }
        Ok(AidResponse {
            task_id,
            stdout: String::from_utf8_lossy(&show_output.stdout).into_owned(),
            cost_usd,
            duration_ms: duration.as_millis() as u64,
        })
    }
}

pub fn parse_task_id(combined_output: &str) -> Option<String> {
    combined_output.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("Task ")?;
        let task_id = rest.split_whitespace().next()?;
        let status = rest.split_whitespace().nth(1)?;
        (status == "started" && is_task_id(task_id)).then(|| task_id.to_owned())
    })
}

pub fn parse_done_line(combined_output: &str) -> Option<(Duration, f64)> {
    combined_output.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("Task ")?;
        let task_id = rest.split_whitespace().next()?;
        let status = rest.split_whitespace().nth(1)?;
        if status != "DONE" || !is_task_id(task_id) {
            return None;
        }
        let open = line.find('(')?;
        let close = line.rfind(')')?;
        let mut parts = line[open + 1..close].split(',').map(str::trim);
        let duration = parse_duration(parts.next()?)?;
        parts.next()?;
        let cost_usd = match parts.next() {
            Some(cost) => parse_cost(cost)?,
            None => 0.0,
        };
        Some((duration, cost_usd))
    })
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    match (stderr.trim(), stdout.trim()) {
        ("", "") => String::new(),
        ("", _) => stdout.into_owned(),
        (_, "") => stderr.into_owned(),
        _ => format!("{stderr}\n{stdout}"),
    }
}

fn format_command_output(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        "no stdout/stderr output".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn parse_duration(text: &str) -> Option<Duration> {
    let mut seconds = 0_u64;
    let mut saw_part = false;
    for token in text.split_whitespace() {
        let (value, unit) = token.split_at(token.len().checked_sub(1)?);
        let value = value.parse::<u64>().ok()?;
        seconds += match unit {
            "h" => value * 60 * 60,
            "m" => value * 60,
            "s" => value,
            _ => return None,
        };
        saw_part = true;
    }
    saw_part.then(|| Duration::from_secs(seconds))
}

fn parse_cost(text: &str) -> Option<f64> {
    text.trim().strip_prefix('$')?.parse::<f64>().ok()
}

fn is_task_id(value: &str) -> bool {
    value.starts_with("t-") && value[2..].chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_id_reads_started_line_from_combined_output() {
        let output = "noise\nTask t-094a started (gemini: prompt)\nTask t-094a DONE (29s, 26566 tokens, $0.03)";
        assert_eq!(parse_task_id(output), Some("t-094a".to_owned()));
    }

    #[test]
    fn parse_task_id_skips_invalid_lines() {
        let output = "Task not-a-task started (gemini)\nTask t-2c39 started (gemini: prompt)";
        assert_eq!(parse_task_id(output), Some("t-2c39".to_owned()));
    }

    #[test]
    fn parse_done_line_reads_seconds_cost() {
        let output = "Task t-094a DONE (29s, 26566 tokens, $0.03)";
        assert_eq!(parse_done_line(output), Some((Duration::from_secs(29), 0.03)));
    }

    #[test]
    fn parse_done_line_reads_minutes_and_seconds() {
        let output = "Task t-abcd DONE (1m 45s, 500k tokens, $1.23)";
        assert_eq!(parse_done_line(output), Some((Duration::from_secs(105), 1.23)));
    }

    #[test]
    fn parse_done_line_defaults_missing_cost_to_zero() {
        let output = "Task t-abcd DONE (5s, 13k tokens)";
        assert_eq!(parse_done_line(output), Some((Duration::from_secs(5), 0.0)));
    }

    #[test]
    fn parse_done_line_reads_longer_duration_format() {
        let output = "Task t-abcd DONE (8m 34s, 665976 tokens, $0.64)";
        assert_eq!(parse_done_line(output), Some((Duration::from_secs(514), 0.64)));
    }

    #[test]
    fn parse_done_line_reads_hours_minutes_seconds() {
        let output = "Task t-abcd DONE (1h 2m 3s, 100 tokens, $2.50)";
        assert_eq!(parse_done_line(output), Some((Duration::from_secs(3723), 2.5)));
    }
}
