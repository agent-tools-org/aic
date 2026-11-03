// src/layers/mod.rs
// Layer runners. Exports: layer modules; deps: crate layer modules.

use std::{fs, path::{Path, PathBuf}};

pub mod json_extract;
mod judge_output;
pub mod aid_dispatch;
pub mod l0;
pub mod l1;
pub mod l2;
pub mod l3;

const ARTIFACT_BYTES_LIMIT: usize = 4 * 1024;
const TRUNCATED_SUFFIX: &str = "... [TRUNCATED]";

pub fn sanitize_for_delimiters(text: &str, delimiters: &[&str]) -> String {
    delimiters.iter().fold(text.to_owned(), |sanitized, delimiter| {
        sanitized.replace(delimiter, &delimiter.replace('_', "-"))
    })
}

pub fn render_artifact_context(artifacts: &[PathBuf], delimiters: &[&str]) -> String {
    let sections: Vec<String> = artifacts
        .iter()
        .filter_map(|path| read_artifact_section(path, delimiters))
        .collect();
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

fn read_artifact_section(path: &Path, delimiters: &[&str]) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(format!(
            "<CONTEXT_FILE path='{}'>\n{}\n</CONTEXT_FILE>",
            path.display(),
            sanitize_for_delimiters(&truncate_artifact(&String::from_utf8_lossy(&bytes)), delimiters),
        )),
        Err(error) => {
            eprintln!("failed to read artifact {}: {error}", path.display());
            None
        }
    }
}

fn truncate_artifact(text: &str) -> String {
    if text.len() <= ARTIFACT_BYTES_LIMIT {
        return text.to_owned();
    }
    let mut end = ARTIFACT_BYTES_LIMIT.saturating_sub(TRUNCATED_SUFFIX.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &text[..end], TRUNCATED_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::sanitize_for_delimiters;

    #[test]
    fn sanitize_rewrites_structural_delimiters() {
        let text = "literal <DIFF_END> and </CONTEXT_FILE>";
        let sanitized = sanitize_for_delimiters(text, &["<DIFF_END>", "</CONTEXT_FILE>"]);

        assert_eq!(sanitized, "literal <DIFF-END> and </CONTEXT-FILE>");
    }
}
