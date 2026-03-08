// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct FindFileTool;

/// Decompose a glob pattern into `(subdirectory_suffix, name_pattern)`.
///
/// `find -name` matches only the filename component, so path structure in the
/// pattern must be converted into a search root adjustment.
///
/// Rules (applied in order):
///   1. Find the **last** `**/` segment — everything before it becomes the
///      subdirectory and everything after it becomes the name pattern.
///   2. If there's no `**` but there is a `/`, split on the last `/`.
///   3. Otherwise the whole pattern is the name pattern.
///
/// Examples:
///   `*.rs`           → ("",    "*.rs")
///   `**/*.rs`        → ("",    "*.rs")
///   `src/**/*.rs`    → ("src", "*.rs")
///   `a/b/**/*.c`     → ("a/b", "*.c")
///   `Cargo.toml`     → ("",    "Cargo.toml")
///   `*lint*`         → ("",    "*lint*")
fn decompose_pattern(pattern: &str) -> (String, String) {
    if let Some(pos) = pattern.rfind("**/") {
        let prefix = pattern[..pos].trim_end_matches('/');
        let name_part = &pattern[pos + 3..];
        return (prefix.to_string(), name_part.to_string());
    }
    if let Some(pos) = pattern.rfind('/') {
        let prefix = &pattern[..pos];
        let name_part = &pattern[pos + 1..];
        return (prefix.to_string(), name_part.to_string());
    }
    (String::new(), pattern.to_string())
}

/// Return `true` when the pattern contains wildcards in a *directory* component
/// such that [`decompose_pattern`] would produce an unusable result (a prefix
/// that itself contains `*`, or an empty name part).
///
/// These patterns require `find -path` instead of `find -name`.
///
/// Examples that need `-path`:
///   `**/sven-team/**`       — all files inside any `sven-team` directory
///   `**/sven-team/**/*.rs`  — .rs files inside any `sven-team` directory
///
/// Examples that stay with `-name` (decompose handles them):
///   `*.rs`          — plain filename glob
///   `**/*.rs`       — recursive filename glob
///   `src/**/*.rs`   — .rs files under `src/` (prefix="src", name="*.rs")
fn is_path_glob(pattern: &str) -> bool {
    let (prefix, name) = decompose_pattern(pattern);
    // decompose_pattern fails (produces an unusable command) when:
    // 1. The derived prefix still contains a wildcard —
    //    e.g. "**/sven-team" from "**/sven-team/**/*.rs" (rfind finds the
    //    last **/, leaving "**/" in the prefix).
    // 2. The derived name still contains '/' —
    //    e.g. "sven-team/**" from "**/sven-team/**" (rfind finds the first
    //    **/ at position 0, leaving "sven-team/**" as the name, which
    //    `find -name` cannot handle).
    // 3. The name is empty (nothing to search for with -name).
    prefix.contains('*') || name.contains('/') || name.is_empty()
}

/// Translate a double-star glob pattern to a `find -path` / `find -ipath` pattern.
///
/// In GNU `find -path`, a single `*` already matches across `/`, so collapsing
/// `**` → `*` is sufficient.  We also ensure the pattern starts with `*/` so
/// that it anchors anywhere in the tree (find reports paths as `./a/b/c`).
fn translate_to_find_path(pattern: &str) -> String {
    // Collapse ** → * (find's * already crosses directory boundaries in -path)
    let pat = pattern.replace("**", "*");
    // Prepend */ if needed so the pattern matches anywhere in the reported path.
    if pat.starts_with('/') || pat.starts_with("*/") {
        pat
    } else {
        format!("*/{pat}")
    }
}

#[async_trait]
impl Tool for FindFileTool {
    fn name(&self) -> &str {
        "find_file"
    }

    fn description(&self) -> &str {
        "Find files by name glob pattern, searching recursively under a root directory.\n\
         Powered by 'find'; excludes .git/, target/, node_modules/, .cargo/registry/.\n\
         Glob patterns:\n\
           '*.rs'               — all .rs files anywhere under root\n\
           '**/*.rs'            — same (**/ prefix is stripped; find is always recursive)\n\
           'src/**/*.rs'        — .rs files under <root>/src/\n\
           '**/sven-team/**'    — all files inside any directory named 'sven-team'\n\
           '**/sven-team/**/*.rs' — .rs files inside any 'sven-team' directory\n\
           'Cargo.toml'         — exact filename anywhere under root\n\
           '*lint*'             — filenames containing 'lint'\n\
         For content search use grep or search_codebase instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Filename glob pattern. Examples: '*.rs', '**/*.toml', 'src/**/*.c', '*lint*', 'Cargo.toml'"
                },
                "root": {
                    "type": "string",
                    "description": "Root directory to search from (default: current directory)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Match filenames case-insensitively (default: false)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 200)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Hard timeout in seconds (default: 10)"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let raw_pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'pattern'"),
        };
        let root = call
            .args
            .get("root")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let case_insensitive = call
            .args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max = call
            .args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize;
        let timeout = call
            .args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);

        let cmd = if is_path_glob(&raw_pattern) {
            // Patterns with wildcards in directory components (e.g. **/dir/**)
            // require `find -path` because `find -name` only matches the filename.
            // GNU find's -path treats * as matching any character including `/`.
            let path_pat = translate_to_find_path(&raw_pattern);
            let path_flag = if case_insensitive { "-ipath" } else { "-path" };

            debug!(
                pattern = %raw_pattern,
                path_pat = %path_pat,
                root = %root,
                "find_file tool (path strategy)"
            );

            format!(
                "timeout {timeout} find {root} {path_flag} '{path_pat}' \
                 -not -path '*/.git/*' \
                 -not -path '*/target/*' \
                 -not -path '*/node_modules/*' \
                 -not -path '*/.cargo/registry/*' \
                 | head -n {max}"
            )
        } else {
            let (subdir, name_pat) = decompose_pattern(&raw_pattern);
            let search_root = if subdir.is_empty() {
                root.clone()
            } else {
                format!("{}/{}", root.trim_end_matches('/'), subdir)
            };
            let name_flag = if case_insensitive { "-iname" } else { "-name" };

            debug!(
                pattern = %raw_pattern,
                search_root = %search_root,
                name_pat = %name_pat,
                "find_file tool (name strategy)"
            );

            format!(
                "timeout {timeout} find {search_root} {name_flag} '{name_pat}' \
                 -not -path '*/.git/*' \
                 -not -path '*/target/*' \
                 -not -path '*/node_modules/*' \
                 -not -path '*/.cargo/registry/*' \
                 | head -n {max}"
            )
        };

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
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
            Err(e) => ToolOutput::err(&call.id, format!("find_file error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "f1".into(),
            name: "find_file".into(),
            args,
        }
    }

    // ── Pattern decomposition ─────────────────────────────────────────────────

    #[test]
    fn decomposes_plain_pattern() {
        assert_eq!(decompose_pattern("*.rs"), ("".into(), "*.rs".into()));
    }

    #[test]
    fn decomposes_double_star_prefix() {
        assert_eq!(decompose_pattern("**/*.rs"), ("".into(), "*.rs".into()));
    }

    #[test]
    fn decomposes_path_with_double_star() {
        assert_eq!(
            decompose_pattern("src/**/*.rs"),
            ("src".into(), "*.rs".into())
        );
    }

    #[test]
    fn decomposes_nested_path_with_double_star() {
        assert_eq!(
            decompose_pattern("a/b/**/*.c"),
            ("a/b".into(), "*.c".into())
        );
    }

    #[test]
    fn decomposes_exact_filename() {
        assert_eq!(
            decompose_pattern("Cargo.toml"),
            ("".into(), "Cargo.toml".into())
        );
    }

    #[test]
    fn decomposes_simple_path() {
        assert_eq!(
            decompose_pattern("crates/lib.rs"),
            ("crates".into(), "lib.rs".into())
        );
    }

    #[test]
    fn decomposes_wildcard_name() {
        assert_eq!(decompose_pattern("*lint*"), ("".into(), "*lint*".into()));
    }

    // ── Search execution ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn finds_toml_files() {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.toml",
                "root": crate_root,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_with_double_star_pattern() {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "**/*.toml",
                "root": crate_root,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_with_subdirectory_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src").join("lib");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("main.rs"), b"fn main() {}").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "src/**/*.rs",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("main.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_in_nested_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("build").join("zephyr");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("zephyr.elf"), b"\x7fELF").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.elf",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("zephyr.elf"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.MD"), b"docs").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.md",
                "root": dir.path().to_str().unwrap(),
                "case_insensitive": true
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("README.MD"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_with_wildcard_name_pattern() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*lint*",
                "root": src,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("read_lints"), "{}", out.content);
    }

    #[tokio::test]
    async fn max_results_is_respected() {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.rs",
                "root": crate_root,
                "max_results": 3
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let lines: Vec<&str> = out.content.lines().collect();
        assert!(lines.len() <= 3, "expected ≤3 results, got {}", lines.len());
    }

    #[tokio::test]
    async fn no_match_returns_no_matches_message() {
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.xyz_nonexistent_ext",
                "root": "/tmp"
            })))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let out = FindFileTool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'pattern'"), "{}", out.content);
    }

    #[test]
    fn schema_requires_only_pattern() {
        let schema = FindFileTool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert!(required.iter().any(|v| v.as_str() == Some("pattern")));
    }

    // ── is_path_glob ──────────────────────────────────────────────────────────

    #[test]
    fn path_glob_detects_dir_wildcard_suffix() {
        // **/dir/** has wildcards in a directory component — needs -path
        assert!(is_path_glob("**/sven-team/**"));
    }

    #[test]
    fn path_glob_detects_dir_wildcard_with_name() {
        // **/dir/**/*.rs — prefix would be "**/dir" (contains *) — needs -path
        assert!(is_path_glob("**/sven-team/**/*.rs"));
    }

    #[test]
    fn path_glob_false_for_plain_pattern() {
        assert!(!is_path_glob("*.rs"));
    }

    #[test]
    fn path_glob_false_for_double_star_filename() {
        // **/*.rs → decompose gives ("", "*.rs") — works fine with -name
        assert!(!is_path_glob("**/*.rs"));
    }

    #[test]
    fn path_glob_false_for_concrete_prefix() {
        // src/**/*.rs → decompose gives ("src", "*.rs") — prefix has no *
        assert!(!is_path_glob("src/**/*.rs"));
    }

    // ── translate_to_find_path ────────────────────────────────────────────────

    #[test]
    fn translates_dir_glob_to_path_pattern() {
        assert_eq!(translate_to_find_path("**/sven-team/**"), "*/sven-team/*");
    }

    #[test]
    fn translates_dir_glob_with_extension() {
        assert_eq!(
            translate_to_find_path("**/sven-team/**/*.rs"),
            "*/sven-team/*/*.rs"
        );
    }

    #[test]
    fn prepends_wildcard_when_no_leading_star() {
        assert_eq!(translate_to_find_path("sven-team/**"), "*/sven-team/*");
    }

    // ── Execute with path-glob patterns ──────────────────────────────────────

    #[tokio::test]
    async fn finds_files_under_dir_anywhere_in_tree() {
        let dir = tempfile::tempdir().unwrap();
        // Create nested: root/crates/sven-team/src/lib.rs
        let sub = dir.path().join("crates").join("sven-team").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("lib.rs"), b"pub fn foo() {}").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "**/sven-team/**",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("lib.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_rs_files_under_dir_anywhere_in_tree() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("crates").join("sven-team").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("lib.rs"), b"pub fn foo() {}").unwrap();
        std::fs::write(sub.join("README.md"), b"# readme").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "**/sven-team/**/*.rs",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("lib.rs"), "{}", out.content);
        assert!(!out.content.contains("README.md"), "{}", out.content);
    }
}
