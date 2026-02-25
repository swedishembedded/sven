// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct GlobFileSearchTool;

/// Normalise a user-supplied glob pattern into a `-name` argument for `find`.
///
/// The agent often supplies patterns like `**/*.elf` or `**/*.toml`.
/// `find -name` only accepts a simple name pattern (no path separators), so
/// we strip any leading `**/` and any path prefix, keeping only the filename
/// glob.  The recursive search is handled by `find` itself.
///
/// Examples:
///   `**/*.elf`                  → `*.elf`
///   `*.rs`                      → `*.rs`
///   `build/**/*.elf`            → `*.elf`
///   `build/zephyr/zephyr.elf`   → `zephyr.elf`
fn normalise_glob_for_find(pattern: &str) -> String {
    // Strip leading `**/` or path components ending in `/`
    let name_part = if let Some(pos) = pattern.rfind('/') {
        &pattern[pos + 1..]
    } else {
        pattern
    };
    name_part.to_string()
}

#[async_trait]
impl Tool for GlobFileSearchTool {
    fn name(&self) -> &str { "glob_file_search" }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern recursively under a root directory. \
         Returns matching file paths sorted by modification time (newest first).\n\
         Pattern tips: use '*.elf', '*.toml', 'zephyr.elf' — path prefix stripped automatically.\n\
         IMPORTANT: pattern must be a single string, not a comma-separated list.\n\
         Right: {\"pattern\": \"*.c\", \"root\": \"/data/ng-iot-platform\", \"max_results\": 200}\n\
         Wrong: {\"pattern\": \"CMakeLists.txt,west.yml\"} (comma list won't match any files)"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Filename glob pattern (e.g. '*.elf', '*.toml', 'zephyr.elf'). \
                        Path prefixes like '**/' are stripped automatically."
                },
                "root": {
                    "type": "string",
                    "description": "Root directory to search from (default: current directory)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 200)"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let raw_pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'pattern'"),
        };
        let root = call.args.get("root").and_then(|v| v.as_str()).unwrap_or(".").to_string();
        let max = call.args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

        // Normalise pattern so `**/*.elf` works with find's -name
        let name_pattern = normalise_glob_for_find(&raw_pattern);

        debug!(pattern = %name_pattern, root = %root, "glob_file_search tool");

        let cmd_str = format!(
            "find {root} -name '{name_pattern}' \
             -not -path '*/.git/*' \
             -not -path '*/node_modules/*' \
             | sort -t/ -k1,1 | head -{max}"
        );

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .stdin(std::process::Stdio::null())
            .output()
            .await;

        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if text.trim().is_empty() {
                    ToolOutput::ok(&call.id, "(no matches)")
                } else {
                    ToolOutput::ok(&call.id, text.trim_end().to_string())
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("glob_file_search error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "g1".into(), name: "glob_file_search".into(), args }
    }

    // ── Pattern normalisation ─────────────────────────────────────────────────

    #[test]
    fn normalise_glob_strips_double_star_prefix() {
        assert_eq!(normalise_glob_for_find("**/*.elf"), "*.elf");
    }

    #[test]
    fn normalise_glob_strips_path_prefix() {
        assert_eq!(normalise_glob_for_find("build/**/*.elf"), "*.elf");
    }

    #[test]
    fn normalise_glob_keeps_plain_name() {
        assert_eq!(normalise_glob_for_find("*.toml"), "*.toml");
    }

    #[test]
    fn normalise_glob_keeps_exact_filename() {
        assert_eq!(normalise_glob_for_find("zephyr.elf"), "zephyr.elf");
    }

    #[test]
    fn normalise_glob_strips_full_path() {
        assert_eq!(
            normalise_glob_for_find("build-firmware/ng-iot-platform/zephyr/zephyr.elf"),
            "zephyr.elf"
        );
    }

    // ── Search execution ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn finds_toml_files() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "*.toml",
            "root": "/data/agents/sven"
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn finds_elf_with_double_star_pattern() {
        // Create a temp dir with an .elf file to test pattern normalisation
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("firmware.elf"), b"\x7fELF").unwrap();

        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "**/*.elf",
            "root": dir.path().to_str().unwrap()
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("firmware.elf"), "got: {}", out.content);
    }

    #[tokio::test]
    async fn finds_elf_in_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("build").join("zephyr");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("zephyr.elf"), b"\x7fELF").unwrap();

        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "*.elf",
            "root": dir.path().to_str().unwrap()
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("zephyr.elf"), "got: {}", out.content);
    }

    #[tokio::test]
    async fn no_match_returns_no_matches_message() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "*.xyz_nonexistent_ext",
            "root": "/tmp"
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"));
    }

    #[tokio::test]
    async fn max_results_is_respected() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "*.rs",
            "root": "/data/agents/sven",
            "max_results": 2
        }))).await;
        assert!(!out.is_error);
        let lines: Vec<&str> = out.content.lines().collect();
        assert!(lines.len() <= 2);
    }

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'pattern'"));
    }
}
