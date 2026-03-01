// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::collections::HashMap;

/// Metadata parsed from simple YAML-style frontmatter at the top of a
/// workflow file.
///
/// Frontmatter is delimited by `---` on its own line:
///
/// ```markdown
/// ---
/// title: My Workflow
/// models:
///   agent: claude-haiku-4-5
///   research: claude-opus-4-5
/// vars:
///   branch: main
///   pr_number: "42"
/// ---
///
/// ## Step one
/// ...
/// ```
///
/// The default agent mode is `agent` and the default model is `claude-haiku-4-5`.
/// Per-mode model overrides (from the `models:` map) take effect when a step
/// uses `<!-- sven: mode=research -->` or similar inline directives.
/// CLI `--model` always takes the highest priority.
///
/// Fields removed compared to the original schema (use CLI flags or config instead):
/// - `mode` (was: override default agent mode)
/// - `model` (was: bare model override — use `models:` map now)
/// - `step_timeout_secs` (was: per-step timeout)
/// - `run_timeout_secs` (was: total run timeout)
#[derive(Debug, Clone, Default)]
pub struct WorkflowMetadata {
    /// Human-readable title (also used as conversation title in output)
    pub title: Option<String>,
    /// Per-mode model map: `mode_name -> model_id`.
    /// For example `{"agent": "claude-haiku-4-5", "research": "claude-opus-4-5"}`.
    /// When a step switches to a mode that has an entry here, that model is
    /// used unless an explicit `--model` CLI flag or inline `model=` step tag
    /// overrides it.
    pub models: Option<HashMap<String, String>>,
    /// Template variables, substituted as `{{key}}` in step content.
    /// Override with CLI `--var KEY=VALUE`; environment variables provide a
    /// final fallback (see `apply_template`).
    pub vars: Option<HashMap<String, String>>,
}

/// Parse optional YAML-style frontmatter from a markdown workflow string.
///
/// Returns `(metadata, remaining_markdown)`.  If no frontmatter is found
/// `metadata` is `None` and `remaining_markdown` is the whole input.
pub fn parse_frontmatter(content: &str) -> (Option<WorkflowMetadata>, &str) {
    // Frontmatter must start at the very first line with "---"
    let header = if let Some(rest) = content.strip_prefix("---\n") {
        rest
    } else if let Some(rest) = content.strip_prefix("---\r\n") {
        rest
    } else {
        return (None, content);
    };

    // Find closing "---" on its own line
    let close_unix = header.find("\n---\n");
    let close_crlf = header.find("\n---\r\n");

    let (yaml_end, md_start_offset) = match (close_unix, close_crlf) {
        (Some(u), Some(c)) if u <= c => (u, "\n---\n".len()),
        (_, Some(c)) => (c, "\n---\r\n".len()),
        (Some(u), _) => (u, "\n---\n".len()),
        (None, None) => return (None, content),
    };

    let yaml_src = &header[..yaml_end];
    let remaining = &header[yaml_end + md_start_offset..];

    match parse_simple_yaml(yaml_src) {
        Some(meta) => (Some(meta), remaining),
        None => (None, content),
    }
}

/// Minimal YAML-subset parser supporting:
/// - Top-level string fields: `key: value` (with optional quotes)
/// - A `vars:` section with indented `  key: value` entries
/// - A `models:` section with indented `  mode: model_id` entries
fn parse_simple_yaml(src: &str) -> Option<WorkflowMetadata> {
    let mut meta = WorkflowMetadata::default();
    // Which top-level section we are currently inside ("vars" | "models" | "")
    let mut current_section = "";
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut models: HashMap<String, String> = HashMap::new();

    for line in src.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }

        // Indented line: belongs to the current section
        if !current_section.is_empty() && (line.starts_with(' ') || line.starts_with('\t')) {
            if let Some((k, v)) = split_kv(line.trim()) {
                match current_section {
                    "vars" => {
                        vars.insert(k, v);
                    }
                    "models" => {
                        models.insert(k, v);
                    }
                    _ => {}
                }
            }
            continue;
        } else if line.starts_with(' ') || line.starts_with('\t') {
            // Indented but no active section — ignore
            continue;
        } else {
            // Non-indented line: close any open section
            current_section = "";
        }

        if let Some((key, val)) = split_kv(line) {
            match key.as_str() {
                "title" => meta.title = Some(val),
                "vars" => {
                    if val.is_empty() {
                        current_section = "vars";
                    }
                }
                "models" => {
                    if val.is_empty() {
                        current_section = "models";
                    }
                }
                // Silently ignore unknown / removed keys for forward compat
                _ => {}
            }
        }
    }

    if !vars.is_empty() {
        meta.vars = Some(vars);
    }
    if !models.is_empty() {
        meta.models = Some(models);
    }

    Some(meta)
}

/// Split `key: value` into `(key, value)`.  Handles quoted values and
/// strips surrounding whitespace / quotes.  Returns `None` if there is no
/// `:` separator or if the key is empty.
fn split_kv(s: &str) -> Option<(String, String)> {
    let colon = s.find(':')?;
    let key = s[..colon].trim().to_string();
    if key.is_empty() {
        return None;
    }
    let raw_val = s[colon + 1..].trim();
    let val = unquote(raw_val).to_string();
    Some((key, val))
}

/// Strip a single layer of matching `"..."` or `'...'` quotes if present.
fn unquote(s: &str) -> &str {
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_returns_none_and_full_content() {
        let md = "## Step\nDo something.";
        let (meta, rest) = parse_frontmatter(md);
        assert!(meta.is_none());
        assert_eq!(rest, md);
    }

    #[test]
    fn well_formed_frontmatter_is_parsed() {
        let md = "---\ntitle: My Workflow\n---\n\n## Step\nDo it.";
        let (meta, rest) = parse_frontmatter(md);
        let meta = meta.expect("frontmatter should be parsed");
        assert_eq!(meta.title.as_deref(), Some("My Workflow"));
        assert!(rest.contains("## Step"));
    }

    #[test]
    fn frontmatter_with_quoted_values() {
        let md = "---\ntitle: \"Quoted Title\"\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Quoted Title"));
    }

    #[test]
    fn frontmatter_with_vars() {
        let md = "---\nvars:\n  branch: main\n  pr: \"42\"\n---\n## Step\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let vars = meta.unwrap().vars.unwrap();
        assert_eq!(vars.get("branch").map(String::as_str), Some("main"));
        assert_eq!(vars.get("pr").map(String::as_str), Some("42"));
    }

    #[test]
    fn frontmatter_with_models_map() {
        let md =
            "---\nmodels:\n  agent: claude-haiku-4-5\n  research: claude-opus-4-5\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let models = meta.unwrap().models.unwrap();
        assert_eq!(
            models.get("agent").map(String::as_str),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            models.get("research").map(String::as_str),
            Some("claude-opus-4-5")
        );
    }

    #[test]
    fn frontmatter_vars_and_models_together() {
        let md = "---\ntitle: Both\nmodels:\n  agent: haiku\nvars:\n  key: value\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Both"));
        assert_eq!(
            m.models
                .as_ref()
                .and_then(|ms| ms.get("agent"))
                .map(String::as_str),
            Some("haiku")
        );
        assert_eq!(
            m.vars
                .as_ref()
                .and_then(|vs| vs.get("key"))
                .map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn missing_closing_delimiter_returns_none() {
        let md = "---\ntitle: oops\n## Step\nno closing delimiter";
        let (meta, rest) = parse_frontmatter(md);
        assert!(meta.is_none());
        assert_eq!(rest, md);
    }

    #[test]
    fn models_then_vars_both_parsed() {
        // Section boundary: models: followed by vars:
        let md = "---\nmodels:\n  agent: haiku\nvars:\n  key: val\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert_eq!(
            m.models
                .as_ref()
                .and_then(|ms| ms.get("agent"))
                .map(String::as_str),
            Some("haiku")
        );
        assert_eq!(
            m.vars
                .as_ref()
                .and_then(|vs| vs.get("key"))
                .map(String::as_str),
            Some("val")
        );
    }

    #[test]
    fn vars_then_models_both_parsed() {
        // Reversed order to verify section-close-on-non-indent works
        let md = "---\nvars:\n  key: val\nmodels:\n  agent: haiku\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert_eq!(
            m.vars
                .as_ref()
                .and_then(|vs| vs.get("key"))
                .map(String::as_str),
            Some("val")
        );
        assert_eq!(
            m.models
                .as_ref()
                .and_then(|ms| ms.get("agent"))
                .map(String::as_str),
            Some("haiku")
        );
    }

    #[test]
    fn models_with_inline_value_is_ignored() {
        // `models: something` on a single line (not a sub-section) should be ignored
        let md = "---\nmodels: invalid-inline-value\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert!(m.models.is_none(), "inline models: value should be ignored");
    }

    #[test]
    fn multiple_modes_in_models_map() {
        let md = "---\nmodels:\n  agent: claude-haiku-4-5\n  research: claude-opus-4-5\n  plan: claude-sonnet-4-5\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let models = meta.unwrap().models.unwrap();
        assert_eq!(
            models.get("agent").map(String::as_str),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            models.get("research").map(String::as_str),
            Some("claude-opus-4-5")
        );
        assert_eq!(
            models.get("plan").map(String::as_str),
            Some("claude-sonnet-4-5")
        );
    }

    #[test]
    fn remaining_markdown_is_correct() {
        // Verify the body split is exact — no bytes dropped or duplicated
        let md = "---\ntitle: T\n---\n# Heading\nContent here.";
        let (meta, rest) = parse_frontmatter(md);
        assert!(meta.is_some());
        assert_eq!(rest, "# Heading\nContent here.");
    }

    #[test]
    fn removed_fields_are_ignored_gracefully() {
        // Old workflows may still have mode/model/timeout fields; they should
        // be parsed without error and silently dropped.
        let md = "---\ntitle: Legacy\nmode: agent\nmodel: claude-opus-4-5\nstep_timeout_secs: 120\nrun_timeout_secs: 600\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Legacy"));
        // Old fields are gone — not accessible
        assert!(m.models.is_none());
    }

    #[test]
    fn unquote_double_quoted() {
        assert_eq!(unquote("\"hello\""), "hello");
    }

    #[test]
    fn unquote_single_quoted() {
        assert_eq!(unquote("'world'"), "world");
    }

    #[test]
    fn unquote_unquoted_is_unchanged() {
        assert_eq!(unquote("plain"), "plain");
    }
}
