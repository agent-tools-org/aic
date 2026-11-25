// src/layers/json_extract.rs
// JSON extraction helpers for LLM output parsing. Exports: extract_json_object, repair_partial_json.
// Deps: serde_json for schema-aware repair plus std string slicing.

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaHint { L2, L3 }
const KNOWN_SCHEMA_HINT_FIELDS: [&str; 5] = ["verdict", "violated_rules", "bugs", "reason", "cited_lines"];

pub fn extract_json_object(s: &str) -> Option<&str> {
    let text = strip_code_fence(s.trim());
    let start = text.find('{')?;
    let end = matching_object_end(text, start)?;
    Some(&text[start..end])
}

pub fn repair_partial_json(raw: &str, schema_hint: SchemaHint) -> Option<String> {
    let text = strip_code_fence(raw.trim());
    if !text.starts_with('{') {
        return None;
    }
    if let Some(json) = extract_json_object(text) {
        if let Some(repaired) = repair_extracted_object(json, schema_hint) {
            return Some(repaired);
        }
    }
    if let Some(violated_rules) = extract_violated_rules(text) {
        return Some(synthesize_partial(text, schema_hint, violated_rules));
    }
    None
}

fn strip_code_fence(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else { return s };
    let Some(newline) = rest.find('\n') else { return s };
    let inner = &rest[newline + 1..];
    let trimmed = inner.trim_end();
    trimmed.strip_suffix("```").map(str::trim).unwrap_or(s)
}

fn matching_object_end(s: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in s[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset + ch.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

fn repair_extracted_object(json_text: &str, schema_hint: SchemaHint) -> Option<String> {
    let value = serde_json::from_str::<Value>(json_text).ok()?;
    let object = value.as_object()?;
    if matches!(object.get("verdict"), Some(Value::String(_))) {
        return Some(json_text.to_owned());
    }
    if !object.keys().any(|key| KNOWN_SCHEMA_HINT_FIELDS.contains(&key.as_str())) {
        return None;
    }
    let violated_rules = extract_rules_from_value(&value);
    let bugs = extract_bugs_from_value(&value);
    if !violated_rules.is_empty() || !bugs.is_empty() {
        return Some(synthesize_partial(json_text, schema_hint, violated_rules));
    }
    Some(synthesize_inconclusive(json_text, schema_hint, &value))
}

fn extract_rules_from_value(value: &Value) -> Vec<String> {
    value.get("violated_rules")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(|item| item.trim().to_owned()).filter(|item| !item.is_empty()).collect())
        .unwrap_or_default()
}

fn extract_bugs_from_value(value: &Value) -> Vec<String> {
    value.get("bugs")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|item| item.get("bug").unwrap_or(item).as_str()).map(|item| item.trim().to_owned()).filter(|item| !item.is_empty()).collect())
        .unwrap_or_default()
}

fn extract_violated_rules(text: &str) -> Option<Vec<String>> {
    let start = text.find("violated_rules=[")?;
    let rest = &text[start + "violated_rules=[".len()..];
    let end = rest.find(']')?;
    let items: Vec<String> = rest[..end]
        .split(',')
        .map(|item| item.trim().trim_matches('`').trim_matches('"').to_owned())
        .filter(|item| !item.is_empty())
        .collect();
    (!items.is_empty()).then_some(items)
}

fn synthesize_partial(raw: &str, schema_hint: SchemaHint, violated_rules: Vec<String>) -> String {
    let detail = format!("partial: {}", preview(raw));
    match schema_hint {
        SchemaHint::L2 => json!({
            "verdict": "fail",
            "reason": detail,
            "cited_lines": [],
        })
        .to_string(),
        SchemaHint::L3 => json!({
            "verdict": "fail",
            "bugs": [{"impact": "medium", "bug": detail, "trigger": "partial judge response"}],
            "violated_rules": violated_rules,
        })
        .to_string(),
    }
}

fn synthesize_inconclusive(raw: &str, schema_hint: SchemaHint, value: &Value) -> String {
    let detail = value.get("reason").and_then(Value::as_str).map(str::trim).filter(|reason| !reason.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("partial: {}", preview(raw)));
    match schema_hint {
        SchemaHint::L2 => json!({
            "verdict": "inconclusive",
            "reason": detail,
            "cited_lines": [],
        })
        .to_string(),
        SchemaHint::L3 => json!({
            "verdict": "inconclusive",
            "bugs": [{"impact": "low", "bug": detail, "trigger": ""}],
            "violated_rules": [],
        })
        .to_string(),
    }
}

fn preview(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = String::new();
    for ch in collapsed.chars().take(160) {
        preview.push(ch);
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::{extract_json_object, repair_partial_json, SchemaHint};
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct L2Response {
        verdict: String,
        reason: String,
        #[allow(dead_code)] cited_lines: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    struct L3Response {
        verdict: String,
        bugs: Vec<serde_json::Value>,
        violated_rules: Vec<String>,
    }

    #[test]
    fn extracts_pure_json() {
        assert_eq!(extract_json_object("{\"verdict\":\"pass\"}"), Some("{\"verdict\":\"pass\"}"));
    }

    #[test]
    fn extracts_markdown_fenced_json() {
        let fenced = "```json\n{\"verdict\":\"pass\"}\n```";
        assert_eq!(extract_json_object(fenced), Some("{\"verdict\":\"pass\"}"));
    }

    #[test]
    fn extracts_json_after_preamble() {
        let output = "Sure, here is the result: {\"verdict\":\"fail\"}";
        assert_eq!(extract_json_object(output), Some("{\"verdict\":\"fail\"}"));
    }

    #[test]
    fn returns_none_when_no_braces_exist() {
        assert_eq!(extract_json_object("No JSON here."), None);
    }

    #[test]
    fn handles_braces_inside_json_strings() {
        let output = "{\"reason\":\"brace } inside string\",\"verdict\":\"pass\"}";
        assert_eq!(extract_json_object(output), Some(output));
    }

    #[test]
    fn repairs_well_formed_json_unchanged() {
        let json = "{\"verdict\":\"pass\",\"reason\":\"no violations\",\"cited_lines\":[]}";
        assert_eq!(repair_partial_json(json, SchemaHint::L2), Some(json.to_owned()));
    }

    #[test]
    fn tool_call_payloads_return_none() {
        for raw in ["{\"dir_path\":\"/tmp\"}", "{\"file_path\":\"foo.md\"}", "{\"pattern\":\"X\"}"] {
            assert_eq!(repair_partial_json(raw, SchemaHint::L2), None);
        }
    }

    #[test]
    fn schema_hint_only_payloads_become_inconclusive() {
        let cited: L2Response = serde_json::from_str(&repair_partial_json("{\"cited_lines\":[]}", SchemaHint::L2).expect("repair")).expect("parse");
        let reason: L2Response = serde_json::from_str(&repair_partial_json("{\"reason\":\"looks sketchy\"}", SchemaHint::L2).expect("repair")).expect("parse");
        assert_eq!(cited.verdict, "inconclusive");
        assert!(cited.reason.contains("partial:"));
        assert_eq!(reason.verdict, "inconclusive");
        assert_eq!(reason.reason, "looks sketchy");
    }

    #[test]
    fn rejects_real_failure_with_rule_suffix_for_l3() {
        let raw = "Reviewing the diff for behavioral regressions and checking the surrounding WooFi cache logic before I decide whether this change is safe.\nviolated_rules=[cached-timestamp-freshness, comment-behavior-parity]";
        assert_eq!(repair_partial_json(raw, SchemaHint::L3), None);
    }

    #[test]
    fn rejects_real_failure_investigation_preamble_for_l3() {
        let raw = "I will begin by investigating the `crates/sr-pc-exotic/src/woofi_cache.rs` file to understand the context of the `is_oracle_fresh` function, specifically how `now` and `FRESHNESS_MARGIN` are defined.";
        assert_eq!(repair_partial_json(raw, SchemaHint::L3), None);
    }

    #[test]
    fn rejects_real_failure_result_file_preamble_for_l3() {
        let raw = "I’ll review the WooFi oracle freshness change against the surrounding implementation and on-chain semantics implied by the code, then write the audit result to `result-t-b809.md`.";
        assert_eq!(repair_partial_json(raw, SchemaHint::L3), None);
    }

    #[test]
    fn repairs_markdown_fenced_json() {
        let fenced = "```json\n{\"verdict\":\"pass\",\"reason\":\"ok\",\"cited_lines\":[]}\n```";
        let repaired = repair_partial_json(fenced, SchemaHint::L2).expect("repair");
        let parsed: L2Response = serde_json::from_str(&repaired).expect("parse");
        assert_eq!(parsed.verdict, "pass");
    }

    #[test]
    fn rejects_preamble_then_json() {
        let raw = "Sure, here is the result: {\"verdict\":\"fail\",\"bugs\":[],\"violated_rules\":[]}";
        assert_eq!(repair_partial_json(raw, SchemaHint::L3), None);
    }

    #[test]
    fn violated_rules_payload_still_repairs_to_fail() {
        let parsed: L3Response = serde_json::from_str(&repair_partial_json("{\"violated_rules\":[\"r1\",\"r2\"]}", SchemaHint::L3).expect("repair")).expect("parse");
        assert_eq!(parsed.verdict, "fail");
        assert_eq!(parsed.violated_rules, vec!["r1", "r2"]);
        assert_eq!(parsed.bugs[0]["impact"], "medium");
        assert!(parsed.bugs[0]["bug"].as_str().expect("bug text").contains("partial:"));
    }

    #[test]
    fn pure_prose_without_hint_returns_none() {
        assert_eq!(repair_partial_json("No structured signal here.", SchemaHint::L3), None);
    }
}
