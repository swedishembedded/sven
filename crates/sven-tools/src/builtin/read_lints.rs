// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

pub struct ReadLintsTool;

#[async_trait]
impl Tool for ReadLintsTool {
    fn name(&self) -> &str {
        "read_lints"
    }

    fn description(&self) -> &str {
        "Get linter diagnostics for files or directories. No paths → whole workspace.\n\
         workdir: set to project root for correct relative path resolution.\n\
         ONLY call on files you've just edited — pre-existing errors will also appear.\n\
         Prefer paths=[specific file or dir you just changed] to avoid noise."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Specific files or directories to lint (optional; defaults to project root)"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory for the lint command (default: current directory)"
                }
            },
            "required": ["paths", "workdir"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let workdir = call
            .args
            .get("workdir")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let paths: Vec<String> = call
            .args
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        debug!(workdir = %workdir, "read_lints tool");

        let project_type = detect_project_type(&workdir).await;
        let mut results: Vec<String> = Vec::new();

        match project_type.as_deref() {
            Some("rust") => {
                let out = run_cargo_check(&workdir).await;
                results.push(out);
            }
            Some("typescript") => {
                let out = run_tsc(&workdir, &paths).await;
                results.push(out);
            }
            Some("python") => {
                let out = run_ruff(&workdir, &paths).await;
                results.push(out);
            }
            _ => {
                // Try all detectable linters
                let rust_out = run_cargo_check(&workdir).await;
                if !rust_out.contains("error: could not find") && !rust_out.trim().is_empty() {
                    results.push(format!("[rust]\n{rust_out}"));
                }
                let ruff_out = run_ruff(&workdir, &paths).await;
                if !ruff_out.trim().is_empty() && !ruff_out.contains("No such file") {
                    results.push(format!("[python]\n{ruff_out}"));
                }
            }
        }

        let output = results.join("\n\n");
        if output.trim().is_empty() {
            ToolOutput::ok(&call.id, "(no diagnostics)")
        } else {
            ToolOutput::ok(&call.id, output)
        }
    }
}

async fn detect_project_type(workdir: &str) -> Option<String> {
    let dir = std::path::Path::new(workdir);

    // Walk up looking for known project files
    let mut current = dir;
    loop {
        if current.join("Cargo.toml").exists() {
            return Some("rust".to_string());
        }
        if current.join("package.json").exists() {
            return Some("typescript".to_string());
        }
        if current.join("pyproject.toml").exists() || current.join("setup.py").exists() {
            return Some("python".to_string());
        }
        match current.parent() {
            Some(p) => current = p,
            None => break,
        }
    }
    None
}

async fn run_cargo_check(workdir: &str) -> String {
    let output = tokio::process::Command::new("cargo")
        .args(["check", "--message-format", "short"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let combined = format!("{stdout}{stderr}");
            // Filter to only error/warning lines
            let filtered: Vec<&str> = combined
                .lines()
                .filter(|l| l.contains("error") || l.contains("warning"))
                .collect();
            if filtered.is_empty() {
                "(no issues)".to_string()
            } else {
                filtered.join("\n")
            }
        }
        Err(e) => format!("cargo check failed: {e}"),
    }
}

async fn run_tsc(workdir: &str, _paths: &[String]) -> String {
    let output = tokio::process::Command::new("npx")
        .args(["tsc", "--noEmit", "--pretty", "false"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = format!("{stdout}{stderr}");
            if combined.trim().is_empty() {
                "(no issues)".to_string()
            } else {
                combined
            }
        }
        Err(e) => format!("tsc failed: {e}"),
    }
}

async fn run_ruff(workdir: &str, paths: &[String]) -> String {
    let mut args = vec![
        "check".to_string(),
        "--output-format".to_string(),
        "concise".to_string(),
    ];
    if paths.is_empty() {
        args.push(".".to_string());
    } else {
        args.extend_from_slice(paths);
    }

    let output = tokio::process::Command::new("ruff")
        .args(&args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.trim().is_empty() {
                "(no issues)".to_string()
            } else {
                stdout.to_string()
            }
        }
        Err(e) => format!("ruff failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "rl1".into(),
            name: "read_lints".into(),
            args,
        }
    }

    #[tokio::test]
    async fn runs_on_sven_codebase() {
        let t = ReadLintsTool;
        let out = t
            .execute(&call(json!({
                "workdir": "/data/agents/sven"
            })))
            .await;
        // Should succeed even if there are lint warnings
        assert!(!out.is_error, "{}", out.content);
    }

    #[tokio::test]
    async fn no_workdir_defaults_gracefully() {
        let t = ReadLintsTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(!out.is_error, "{}", out.content);
    }
}
