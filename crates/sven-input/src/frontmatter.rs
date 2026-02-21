use std::collections::HashMap;

/// Metadata parsed from simple YAML-style frontmatter at the top of a
/// workflow file.
///
/// Frontmatter is delimited by `---` on its own line:
///
/// ```markdown
/// ---
/// title: My Workflow
/// mode: agent
/// model: anthropic/claude-opus-4-5
/// step_timeout_secs: 300
/// run_timeout_secs: 600
/// vars:
///   branch: main
///   pr_number: "42"
/// ---
///
/// ## Step one
/// ...
/// ```
#[derive(Debug, Clone, Default)]
pub struct WorkflowMetadata {
    /// Human-readable title (also used as conversation title in output)
    pub title: Option<String>,
    /// Override default agent mode ("research" | "plan" | "agent")
    pub mode: Option<String>,
    /// Override model ("gpt-4o", "anthropic/claude-opus-4-5", etc.)
    pub model: Option<String>,
    /// Per-step timeout in seconds (0 = no limit)
    pub step_timeout_secs: Option<u64>,
    /// Total run timeout in seconds (0 = no limit)
    pub run_timeout_secs: Option<u64>,
    /// Template variables, substituted as `{{key}}` in step content
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

/// Minimal YAML-subset parser that handles the specific fields used in
/// workflow frontmatter.  Supports:
/// - Top-level string fields: `key: value` (with optional quotes)
/// - Top-level integer fields: `key: 123`
/// - A `vars:` section with indented `  key: value` entries
fn parse_simple_yaml(src: &str) -> Option<WorkflowMetadata> {
    let mut meta = WorkflowMetadata::default();
    let mut in_vars = false;
    let mut vars: HashMap<String, String> = HashMap::new();

    for line in src.lines() {
        // Skip comment lines and blank lines
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }

        // Indented line inside vars: section
        if in_vars {
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some((k, v)) = split_kv(line.trim()) {
                    vars.insert(k, v);
                }
                continue;
            } else {
                // No longer indented — leave vars section
                in_vars = false;
            }
        }

        if let Some((key, val)) = split_kv(line) {
            match key.as_str() {
                "title" => meta.title = Some(val),
                "mode" => meta.mode = Some(val),
                "model" => meta.model = Some(val),
                "step_timeout_secs" => meta.step_timeout_secs = val.parse().ok(),
                "run_timeout_secs" => meta.run_timeout_secs = val.parse().ok(),
                "vars" => {
                    // `vars:` line with no value — sub-section follows
                    if val.is_empty() {
                        in_vars = true;
                    }
                }
                _ => {}
            }
        }
    }

    if !vars.is_empty() {
        meta.vars = Some(vars);
    }

    Some(meta)
}

/// Split `key: value` into `(key, value)`.  Handles quoted values and
/// strips surrounding whitespace / quotes.  Returns `None` if there is no
/// `: ` separator.
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
    if (s.starts_with('"') && s.ends_with('"'))
        || (s.starts_with('\'') && s.ends_with('\''))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Extract the first H1 title from a markdown string (line starting with `# `
/// that is not `## `).  Used as a fallback title when frontmatter is absent.
pub fn extract_h1_title(md: &str) -> Option<String> {
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with("# ") && !t.starts_with("## ") {
            return Some(t[2..].trim().to_string());
        }
        if t.starts_with("## ") {
            break;
        }
    }
    None
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
        let md = "---\ntitle: My Workflow\nmode: agent\n---\n\n## Step\nDo it.";
        let (meta, rest) = parse_frontmatter(md);
        let meta = meta.expect("frontmatter should be parsed");
        assert_eq!(meta.title.as_deref(), Some("My Workflow"));
        assert_eq!(meta.mode.as_deref(), Some("agent"));
        assert!(rest.contains("## Step"));
    }

    #[test]
    fn frontmatter_with_quoted_values() {
        let md = "---\ntitle: \"Quoted Title\"\nmodel: 'anthropic/claude-3'\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Quoted Title"));
        assert_eq!(m.model.as_deref(), Some("anthropic/claude-3"));
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
    fn frontmatter_timeouts_are_parsed() {
        let md = "---\nstep_timeout_secs: 120\nrun_timeout_secs: 600\n---\n## s\ngo.";
        let (meta, _) = parse_frontmatter(md);
        let m = meta.unwrap();
        assert_eq!(m.step_timeout_secs, Some(120));
        assert_eq!(m.run_timeout_secs, Some(600));
    }

    #[test]
    fn missing_closing_delimiter_returns_none() {
        let md = "---\ntitle: oops\n## Step\nno closing delimiter";
        let (meta, rest) = parse_frontmatter(md);
        assert!(meta.is_none());
        assert_eq!(rest, md);
    }

    #[test]
    fn extract_h1_from_preamble() {
        let md = "# My Project\n\n## Step one\nDo it.";
        assert_eq!(extract_h1_title(md).as_deref(), Some("My Project"));
    }

    #[test]
    fn no_h1_returns_none() {
        let md = "## Step one\nDo it.";
        assert!(extract_h1_title(md).is_none());
    }

    #[test]
    fn h2_does_not_count_as_h1() {
        let md = "## Step one\nDo it.";
        assert!(extract_h1_title(md).is_none());
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
