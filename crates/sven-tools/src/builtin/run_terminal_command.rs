// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

const OUTPUT_LIMIT: usize = 100_000;

pub struct RunTerminalCommandTool {
    pub timeout_secs: u64,
}

impl Default for RunTerminalCommandTool {
    fn default() -> Self {
        Self { timeout_secs: 30 }
    }
}

#[async_trait]
impl Tool for RunTerminalCommandTool {
    fn name(&self) -> &str { "run_terminal_command" }

    fn description(&self) -> &str {
        "Executes a given command in a shell session.\n\n\
         IMPORTANT: This tool is for terminal operations like git, cargo, make, etc. \
         DO NOT use it for file operations — use specialized tools instead.\n\n\
         ## Before Executing\n\
         1. Directory Verification: Before creating directories or files, verify the parent exists.\n\
            - Run 'ls /parent' before 'mkdir /parent/new'\n\
         2. Command Execution: Always quote paths that contain spaces.\n\
            - Good: command=\"ls \\\"/path with spaces\\\"\"\n\
            - Bad:  command=\"ls /path with spaces\"\n\n\
         ## File Operation Prohibition\n\
         VERY IMPORTANT: You MUST avoid using these for file operations:\n\
         - DO NOT use cat, head, tail → use read_file tool\n\
         - DO NOT use grep or find   → use grep and glob tools\n\
         - DO NOT use sed or awk     → use edit_file tool\n\
         If you still need to search in a terminal command, use 'rg' (ripgrep), not 'grep'.\n\n\
         ## Parallel vs Sequential Commands\n\
         - Independent commands: call run_terminal_command multiple times in the same turn (parallel)\n\
         - Dependent commands: chain with '&&' in a single call\n\
         - Use ';' only when you need sequential execution but don't care about failures\n\n\
         ## Long-Running Commands\n\
         - Default timeout is 30 seconds; set timeout_secs higher for slow builds or tests\n\
         - If a command times out, increase timeout_secs and retry\n\
         - Avoid running persistent servers or watchers; prefer one-shot commands\n\n\
         ## Git Safety Protocol\n\
         - NEVER update the git config\n\
         - NEVER run destructive/irreversible commands (push --force, reset --hard) without explicit request\n\
         - NEVER skip hooks (--no-verify, --no-gpg-sign) without explicit user permission\n\
         - NEVER force push to main/master without explicit request\n\
         - Avoid git commit --amend. ONLY use --amend when ALL three conditions are met:\n\
           1. User explicitly requested it, OR commit succeeded but hook auto-modified files\n\
           2. HEAD commit was created by you in this conversation\n\
           3. Commit has NOT been pushed to remote\n\
         - CRITICAL: If commit FAILED or was REJECTED by hook, NEVER amend — fix and create a NEW commit\n\
         - CRITICAL: If already pushed to remote, NEVER amend unless user explicitly requests it\n\
         - NEVER commit unless explicitly asked by user\n\n\
         ## Commit Workflow\n\
         When user requests a commit, first run these in parallel:\n\
         - 'git status' to see all changed/untracked files\n\
         - 'git diff' to see staged and unstaged changes\n\
         - 'git log -5 --oneline' to understand this repository's commit style\n\
         Then:\n\
         1. Stage specific files: 'git add <file1> <file2>' (avoid 'git add -A' or 'git add .')\n\
         2. Write a concise commit message (1-2 sentences, focus on 'why' not 'what')\n\
            Do not commit files that may contain secrets (.env, credentials.json, etc.)\n\
         3. Use HEREDOC format for reliable formatting:\n\
            git commit -m \"$(cat <<'EOF'\n\
            Your commit message here.\n\n\
            EOF\n\
            )\"\n\
         4. Verify with 'git status' — do not create empty commits\n\
         CRITICAL: NEVER push unless explicitly requested by user.\n\n\
         ## Merge Request Creation (GitLab)\n\
         Use the glab CLI for all GitLab tasks (issues, MRs, pipelines, CI).\n\
         When creating an MR, first run in parallel:\n\
         - 'git status', 'git diff', 'git log [base]...HEAD'\n\
         Then:\n\
         1. Push the branch: 'git push -u origin HEAD'\n\
         2. Create the MR: glab mr create --title \"...\" --description \"$(cat <<'EOF'\n\
            ## Summary\n\
            <1-3 bullet points>\n\n\
            ## Test Plan\n\
            <checklist>\n\n\
            Closes #<issue_number>\n\
            EOF\n\
            )\"\n\
         3. Return the MR URL to the user\n\n\
         ## Examples\n\
         <example>\n\
         Good — build and test:\n\
         command=\"cargo test\", workdir=\"/project\"\n\
         </example>\n\
         <example>\n\
         BAD — use read_file instead:\n\
         command=\"cat src/main.rs\"\n\
         </example>\n\
         <example>\n\
         BAD — use grep tool instead:\n\
         command=\"grep -r 'fn main' src/\"\n\
         </example>\n\n\
         ## IMPORTANT\n\
         - Output is limited to 100,000 characters and will be truncated if exceeded\n\
         - Default timeout is 30 seconds; set timeout_secs for longer operations\n\
         - Set workdir to project_root for commands to execute in the correct directory\n\
         - Non-zero exit codes are returned as errors; check the exit code in output"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory (optional, defaults to cwd)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Execution timeout in seconds (optional)"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Ask }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let command = match call.args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'command' argument"),
        };
        let workdir = call.args.get("workdir").and_then(|v| v.as_str()).map(str::to_string);
        let timeout = call.args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs);

        debug!(cmd = %command, "run_terminal_command tool");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command);
        if let Some(wd) = &workdir {
            cmd.current_dir(wd);
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            cmd.output(),
        ).await;

        match result {
            Ok(Ok(output)) => {
                let mut content = String::new();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !stdout.is_empty() {
                    content.push_str(&truncate(&stdout, OUTPUT_LIMIT));
                }
                if !stderr.is_empty() {
                    if !content.is_empty() { content.push('\n'); }
                    content.push_str("[stderr]\n");
                    content.push_str(&truncate(&stderr, OUTPUT_LIMIT));
                }
                if content.is_empty() {
                    content = format!("[exit {}]", output.status.code().unwrap_or(-1));
                }

                if output.status.success() {
                    ToolOutput::ok(&call.id, content)
                } else {
                    let code = output.status.code().unwrap_or(-1);
                    ToolOutput::err(&call.id, format!("[exit {code}]\n{content}"))
                }
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("spawn error: {e}")),
            Err(_) => ToolOutput::err(&call.id, format!("timeout after {timeout}s")),
        }
    }
}

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}...[truncated {} bytes]", &s[..limit], s.len() - limit)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "run_terminal_command".into(), args }
    }

    #[tokio::test]
    async fn executes_echo_and_returns_stdout() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({"command": "echo hello"}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn captures_stderr() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({"command": "echo err >&2"}))).await;
        assert!(out.content.contains("err"));
    }

    #[tokio::test]
    async fn non_zero_exit_is_error() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({"command": "exit 1"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("[exit 1]"));
    }

    #[tokio::test]
    async fn missing_command_is_error() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'command'"));
    }

    #[tokio::test]
    async fn timeout_returns_error() {
        let t = RunTerminalCommandTool { timeout_secs: 1 };
        let out = t.execute(&call(json!({"command": "sleep 60", "timeout_secs": 1}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("timeout"));
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = RunTerminalCommandTool::default();
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }
}
