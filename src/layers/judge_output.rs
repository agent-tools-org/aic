//! Final-answer JSON validation shared by the L2 and L3 judges.
//! Uses serde_json and the existing partial-object repair helper.
use super::json_extract::{extract_json_object, repair_partial_json, SchemaHint};
use serde_json::Value;

/// The final answer block when it is exactly one complete JSON object, with no repair.
pub(crate) fn strict_final_json(output: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(strip_fence(final_block(output).trim())).ok()?;
    value.is_object().then_some(value)
}

fn final_block(output: &str) -> &str {
    let mut final_block = output;
    let mut offset = 0;
    for line in output.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            final_block = &output[offset + line.len()..];
        }
        offset += line.len();
    }
    final_block
}

pub(super) fn judge_json(output: &str, hint: SchemaHint) -> Option<String> {
    judge_json_marked(output, hint).map(|(json, _)| json)
}

/// The judge's final JSON object and whether it had to be repaired from a partial answer.
pub(super) fn judge_json_marked(output: &str, hint: SchemaHint) -> Option<(String, bool)> {
    let block = strip_fence(final_block(output).trim());
    if !block.starts_with('{') {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(block) {
        return value.is_object().then(|| (block.to_owned(), false));
    }
    // A complete object followed by prose or another object is not a single JSON answer.
    if extract_json_object(block).is_some() {
        return None;
    }
    // Prose never reaches repair. A truncated object must not synthesize a passing verdict.
    let repaired = repair_partial_json(block, hint)
        .or_else(|| repair_partial_json(&format!("{block}}}"), hint))?;
    let value: Value = serde_json::from_str(&repaired).ok()?;
    (value.get("verdict")?.as_str()? != "pass").then_some((repaired, true))
}

fn strip_fence(block: &str) -> &str {
    let Some(inner) = block.strip_prefix("```") else {
        return block;
    };
    let Some((_, rest)) = inner.split_once('\n') else {
        return block;
    };
    rest.trim_end()
        .strip_suffix("```")
        .map(str::trim)
        .unwrap_or(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_single_final_object() {
        let json = r#"{"verdict":"pass"}"#;
        for raw in [
            format!("{json} trailing"),
            format!("{json}\n{json}"),
            format!("prose {json}"),
            format!("{json}\n---\nNo answer"),
        ] {
            assert!(judge_json(&raw, SchemaHint::L2).is_none(), "{raw}");
        }
        assert_eq!(
            judge_json(&format!("noise\n---\n```json\n{json}\n```"), SchemaHint::L2),
            Some(json.into())
        );
    }

    #[test]
    fn repairs_truncated_failure_but_never_prose_or_nested_pass() {
        let raw = "{\"violated_rules\":[\"r1\"]\n";
        let repaired = judge_json(raw, SchemaHint::L3).expect("partial failure");
        assert_eq!(
            serde_json::from_str::<Value>(&repaired).unwrap()["verdict"],
            "fail"
        );
        assert!(judge_json("Reviewing the diff violated_rules=[r1]", SchemaHint::L3).is_none());
        assert!(judge_json("{\"example\":{\"verdict\":\"pass\"}", SchemaHint::L3).is_none());
    }
}
