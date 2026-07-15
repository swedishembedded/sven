// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! JSON-argument repair for streaming tool calls.
//!
//! When the LLM streams tool-call arguments, the accumulated JSON is often
//! truncated or malformed (unterminated strings, invalid escapes, missing
//! braces). [`attempt_json_repair`] applies a series of best-effort fixes so a
//! recoverable argument object can still be parsed.
//!
//! Tool-call slot accumulation and dispatch are the responsibility of
//! [`crate::stream_turn`] (accumulation) and the HSM kernel's `ToolExecutor`
//! (execution); this module only owns the repair primitive they share.

/// Attempt to repair common JSON syntax errors in streaming tool arguments.
pub(crate) fn attempt_json_repair(json_str: &str) -> anyhow::Result<serde_json::Value> {
    let fixed = fix_invalid_json_escapes(json_str);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&fixed) {
        return Ok(v);
    }

    let repaired = regex::Regex::new(r#""([^"]+)"([a-zA-Z_][a-zA-Z0-9_]*)":\s*"#)
        .unwrap()
        .replace_all(&fixed, r#""$1", "$2": "#);

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&repaired) {
        return Ok(v);
    }

    if !fixed.trim().ends_with('}') {
        let mut completed = fixed.clone();
        let quote_count = fixed.chars().filter(|&c| c == '"').count();
        if quote_count % 2 == 1 {
            completed.push('"');
        }
        if !completed.trim().ends_with('}') {
            completed.push('}');
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&completed) {
            return Ok(v);
        }
    }

    anyhow::bail!("JSON repair failed: all repair strategies exhausted")
}

fn fix_invalid_json_escapes(json_str: &str) -> String {
    let mut result = String::with_capacity(json_str.len() + 16);
    let mut chars = json_str.chars();
    let mut in_string = false;

    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '\\' => match chars.next() {
                    Some(next)
                        if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') =>
                    {
                        result.push('\\');
                        result.push(next);
                    }
                    Some(next) => {
                        result.push('\\');
                        result.push('\\');
                        result.push(next);
                    }
                    None => result.push('\\'),
                },
                '"' => {
                    in_string = false;
                    result.push('"');
                }
                _ => result.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            result.push(c);
        }
    }
    result
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── JSON repair ───────────────────────────────────────────────────────────

    #[test]
    fn attempt_json_repair_completes_truncated_object() {
        let v = attempt_json_repair(r#"{"x":1"#).unwrap();
        assert_eq!(v["x"], json!(1));
    }

    #[test]
    fn attempt_json_repair_fixes_invalid_escape() {
        let v = attempt_json_repair(r#"{"path":"\c"}"#).unwrap();
        assert_eq!(v["path"], json!("\\c"));
    }

    #[test]
    fn attempt_json_repair_returns_err_on_unrecoverable() {
        assert!(attempt_json_repair("not json at all ~~~").is_err());
    }

    // ── Adversarial JSON repair inputs ────────────────────────────────────────

    #[test]
    fn adversarial_deeply_nested_json_does_not_stack_overflow() {
        // Build 500 levels of nesting: {"a":{"a":{"a": ... }}}
        let open: String = r#"{"a":"#.repeat(500);
        let close: String = "}".repeat(500);
        let deeply_nested = format!("{open}1{close}");
        // Must return a result (Ok or Err) without panicking/stack overflowing.
        let _ = attempt_json_repair(&deeply_nested);
    }

    #[test]
    fn adversarial_100kb_string_value_does_not_panic() {
        let big_val = "x".repeat(100_000);
        let input = format!(r#"{{"key":"{big_val}"}}"#);
        // Valid JSON with a huge string - repair should succeed.
        let result = attempt_json_repair(&input);
        assert!(
            result.is_ok(),
            "100 KB string value should parse: {:?}",
            result
        );
    }

    #[test]
    fn adversarial_mismatched_open_brackets_does_not_panic() {
        for payload in ["{{{", "}}}", "[{]}", "[[[["] {
            let _ = attempt_json_repair(payload);
        }
    }

    #[test]
    fn adversarial_multiple_concatenated_objects_handled() {
        // Two valid objects concatenated - not valid JSON; repair must not panic.
        let _ = attempt_json_repair(r#"{"a":1}{"b":2}"#);
    }

    #[test]
    fn adversarial_trailing_garbage_after_valid_object() {
        // Valid object followed by garbage - serde_json treats trailing bytes as
        // an error, so repair may fail, but must not panic.
        let _ = attempt_json_repair(r#"{"a":1} GARBAGE TEXT"#);
    }

    #[test]
    fn adversarial_only_whitespace_returns_err() {
        assert!(attempt_json_repair("   \t\n  ").is_err());
    }

    #[test]
    fn adversarial_empty_string_returns_err() {
        assert!(attempt_json_repair("").is_err());
    }

    #[test]
    fn adversarial_unicode_null_in_string_does_not_panic() {
        // JSON strings can contain \u0000; the parser must handle it.
        let _ = attempt_json_repair(r#"{"key":"\u0000"}"#);
    }

    #[test]
    fn adversarial_regex_dos_unclosed_string_does_not_hang() {
        // A very long string without a closing quote; the repair regex must not
        // catastrophically backtrack.
        let payload = format!(r#"{{"x":"{}"#, "a".repeat(50_000));
        let _ = attempt_json_repair(&payload);
    }
}
