// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `{{ path }}` prompt / effect template interpolation.
//!
//! Templates are plain strings with `{{ path }}` placeholders.  Paths follow
//! the same root/key syntax as guard expressions:
//!
//! ```text
//! {{ fact.user_request }}
//! {{ decision.status }}
//! {{ event.text }}
//! {{ const.answer_contract }}   ← a named string constant
//! ```
//!
//! Unknown paths render as the empty string (with no panic).  The interpolation
//! is simple string substitution — no logic, no loops, no arithmetic.  Anything
//! more complex belongs in a native function.

use std::collections::HashMap;

use serde_json::Value;

use crate::model::{GuardRoot, PathKey};

// ─── TemplateCtx ─────────────────────────────────────────────────────────────

/// Read-only context for template interpolation.
pub struct TemplateCtx<'a> {
    /// `ctx.facts`.
    pub facts: &'a serde_json::Map<String, Value>,
    /// Optional parsed LLM decision.
    pub decision: Option<&'a Value>,
    /// The triggering event as JSON.
    pub event: &'a Value,
    /// Optional loop-state JSON.
    pub loop_state: Option<&'a Value>,
    /// Named string constants (e.g. large shared prompt fragments).
    pub constants: &'a HashMap<String, String>,
}

// ─── TemplateError ───────────────────────────────────────────────────────────

/// An error from template rendering.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("template syntax error at position {pos}: {message}")]
    Syntax { pos: usize, message: String },
}

// ─── render ──────────────────────────────────────────────────────────────────

/// Interpolate a template string, replacing `{{ path }}` placeholders with
/// values from `ctx`.
///
/// Unknown paths (or paths that resolve to `null`) are replaced with an empty
/// string. This function never returns an error for missing paths; only
/// structural syntax errors in the template (unmatched `{{`) cause `Err`.
pub fn render(template: &str, ctx: &TemplateCtx<'_>) -> Result<String, TemplateError> {
    let mut result = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Look for `{{`.
        if i + 1 < len && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Find the matching `}}`.
            let start = i + 2;
            let end = match template[start..].find("}}") {
                Some(offset) => start + offset,
                None => {
                    return Err(TemplateError::Syntax {
                        pos: i,
                        message: "unmatched '{{'".into(),
                    })
                }
            };
            let placeholder = template[start..end].trim();
            let value = resolve_placeholder(placeholder, ctx);
            result.push_str(&value);
            i = end + 2; // skip past '}}'
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    Ok(result)
}

/// Resolve a single placeholder like `fact.user_request` or `const.answer_contract`.
fn resolve_placeholder(placeholder: &str, ctx: &TemplateCtx<'_>) -> String {
    // Split on the first dot to get the root.
    let (root_str, rest) = match placeholder.find('.') {
        Some(pos) => (&placeholder[..pos], &placeholder[pos + 1..]),
        None => (placeholder, ""),
    };

    match root_str {
        "const" => {
            // Named constant.
            ctx.constants.get(rest).cloned().unwrap_or_default()
        }
        other => {
            // Parse as a guard path and navigate.
            let root = match other {
                "fact" => GuardRoot::Fact,
                "decision" => GuardRoot::Decision,
                "event" => GuardRoot::Event,
                "loop" => GuardRoot::Loop,
                _ => return String::new(),
            };
            let keys = parse_path_keys(rest);
            let value = resolve_path(root, &keys, ctx);
            value_to_string(&value)
        }
    }
}

/// Parse a dotted path like `foo.bar[0].baz` into [`PathKey`] slices.
fn parse_path_keys(path: &str) -> Vec<PathKey> {
    if path.is_empty() {
        return vec![];
    }
    let mut keys = vec![];
    for part in path.split('.') {
        // Handle array indexing: `items[0]`.
        if let Some(bracket) = part.find('[') {
            let field = &part[..bracket];
            if !field.is_empty() {
                keys.push(PathKey::Field(field.to_string()));
            }
            let rest = &part[bracket..];
            let mut s = rest;
            while let Some(open) = s.find('[') {
                if let Some(close) = s[open + 1..].find(']') {
                    let idx_str = &s[open + 1..open + 1 + close];
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        keys.push(PathKey::Index(idx));
                    }
                    s = &s[open + 1 + close + 1..];
                } else {
                    break;
                }
            }
        } else {
            keys.push(PathKey::Field(part.to_string()));
        }
    }
    keys
}

/// Resolve a path in the template context.
fn resolve_path(root: GuardRoot, keys: &[PathKey], ctx: &TemplateCtx<'_>) -> Value {
    match root {
        GuardRoot::Fact => {
            let Some(PathKey::Field(name)) = keys.first() else {
                return Value::Null;
            };
            let base = ctx.facts.get(name.as_str()).cloned().unwrap_or(Value::Null);
            navigate(&base, &keys[1..])
        }
        GuardRoot::Decision => {
            let base = ctx.decision.cloned().unwrap_or(Value::Null);
            navigate(&base, keys)
        }
        GuardRoot::Event => navigate(ctx.event, keys),
        GuardRoot::Loop => {
            let base = ctx.loop_state.cloned().unwrap_or(Value::Null);
            navigate(&base, keys)
        }
        GuardRoot::Retry => Value::Null, // not available in templates
    }
}

/// Navigate a JSON value by path keys.
fn navigate(v: &Value, keys: &[PathKey]) -> Value {
    if keys.is_empty() {
        return v.clone();
    }
    match (&keys[0], v) {
        (PathKey::Field(f), Value::Object(m)) => {
            navigate(m.get(f.as_str()).unwrap_or(&Value::Null), &keys[1..])
        }
        (PathKey::Index(i), Value::Array(a)) => {
            navigate(a.get(*i).unwrap_or(&Value::Null), &keys[1..])
        }
        _ => Value::Null,
    }
}

/// Convert a JSON value to a display string.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn empty_ctx() -> (serde_json::Map<String, Value>, HashMap<String, String>, Value) {
        (serde_json::Map::new(), HashMap::new(), json!({}))
    }

    #[test]
    fn plain_string_unchanged() {
        let (facts, constants, event) = empty_ctx();
        let ctx = TemplateCtx {
            facts: &facts,
            decision: None,
            event: &event,
            loop_state: None,
            constants: &constants,
        };
        assert_eq!(render("hello world", &ctx).unwrap(), "hello world");
    }

    #[test]
    fn interpolates_fact() {
        let mut facts = serde_json::Map::new();
        facts.insert("name".into(), json!("Alice"));
        let constants = HashMap::new();
        let event = json!({});
        let ctx = TemplateCtx {
            facts: &facts,
            decision: None,
            event: &event,
            loop_state: None,
            constants: &constants,
        };
        assert_eq!(render("Hello {{ fact.name }}!", &ctx).unwrap(), "Hello Alice!");
    }

    #[test]
    fn missing_fact_is_empty() {
        let facts = serde_json::Map::new();
        let constants = HashMap::new();
        let event = json!({});
        let ctx = TemplateCtx {
            facts: &facts,
            decision: None,
            event: &event,
            loop_state: None,
            constants: &constants,
        };
        assert_eq!(render("{{ fact.missing }}", &ctx).unwrap(), "");
    }

    #[test]
    fn interpolates_decision() {
        let decision = json!({ "status": "proceed" });
        let facts = serde_json::Map::new();
        let constants = HashMap::new();
        let event = json!({});
        let ctx = TemplateCtx {
            facts: &facts,
            decision: Some(&decision),
            event: &event,
            loop_state: None,
            constants: &constants,
        };
        assert_eq!(
            render("Status: {{ decision.status }}", &ctx).unwrap(),
            "Status: proceed"
        );
    }

    #[test]
    fn interpolates_const() {
        let facts = serde_json::Map::new();
        let mut constants = HashMap::new();
        constants.insert("greeting".into(), "Hello from const!".into());
        let event = json!({});
        let ctx = TemplateCtx {
            facts: &facts,
            decision: None,
            event: &event,
            loop_state: None,
            constants: &constants,
        };
        assert_eq!(render("{{ const.greeting }}", &ctx).unwrap(), "Hello from const!");
    }

    #[test]
    fn unmatched_open_brace_is_error() {
        let facts = serde_json::Map::new();
        let constants = HashMap::new();
        let event = json!({});
        let ctx = TemplateCtx {
            facts: &facts,
            decision: None,
            event: &event,
            loop_state: None,
            constants: &constants,
        };
        assert!(render("{{ unclosed", &ctx).is_err());
    }

    #[test]
    fn event_text() {
        let facts = serde_json::Map::new();
        let constants = HashMap::new();
        let event = json!({ "text": "what is 2+2?" });
        let ctx = TemplateCtx {
            facts: &facts,
            decision: None,
            event: &event,
            loop_state: None,
            constants: &constants,
        };
        assert_eq!(render("User said: {{ event.text }}", &ctx).unwrap(), "User said: what is 2+2?");
    }
}
