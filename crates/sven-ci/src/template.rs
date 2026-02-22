// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::collections::HashMap;

/// Substitute `{{key}}` placeholders in `content` using the provided `vars`.
/// Keys are looked up case-sensitively.  Unknown placeholders are left as-is.
pub fn apply_template(content: &str, vars: &HashMap<String, String>) -> String {
    if vars.is_empty() || !content.contains("{{") {
        return content.to_string();
    }

    let mut result = content.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("{{{{{}}}}}", key), value);
    }
    result
}

/// Parse a `KEY=VALUE` string into a `(key, value)` pair.
/// The key is trimmed; the value is kept verbatim after the first `=`.
pub fn parse_var(spec: &str) -> Option<(String, String)> {
    let (k, v) = spec.split_once('=')?;
    if k.trim().is_empty() {
        return None;
    }
    Some((k.trim().to_string(), v.to_string()))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn simple_substitution() {
        let result = apply_template("Hello {{name}}!", &vars(&[("name", "world")]));
        assert_eq!(result, "Hello world!");
    }

    #[test]
    fn multiple_vars() {
        let result = apply_template(
            "Branch: {{branch}}, PR: {{pr}}",
            &vars(&[("branch", "main"), ("pr", "42")]),
        );
        assert_eq!(result, "Branch: main, PR: 42");
    }

    #[test]
    fn unknown_placeholder_is_left_as_is() {
        let result = apply_template("{{unknown}} stays", &vars(&[]));
        assert_eq!(result, "{{unknown}} stays");
    }

    #[test]
    fn no_vars_returns_content_unchanged() {
        let content = "no placeholders here";
        let result = apply_template(content, &HashMap::new());
        assert_eq!(result, content);
    }

    #[test]
    fn parse_var_simple() {
        let (k, v) = parse_var("branch=main").unwrap();
        assert_eq!(k, "branch");
        assert_eq!(v, "main");
    }

    #[test]
    fn parse_var_value_with_equals() {
        let (k, v) = parse_var("url=https://example.com?a=b").unwrap();
        assert_eq!(k, "url");
        assert_eq!(v, "https://example.com?a=b");
    }

    #[test]
    fn parse_var_no_equals_returns_none() {
        assert!(parse_var("noequalssign").is_none());
    }

    #[test]
    fn parse_var_empty_key_returns_none() {
        assert!(parse_var("=value").is_none());
    }
}
