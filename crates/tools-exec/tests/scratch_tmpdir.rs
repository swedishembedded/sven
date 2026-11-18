// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a shell given the session's scratch directory hands it to every
//! command as `TMPDIR`, so `mktemp` and every program that honours it keep
//! their temporary files there instead of the shared `/tmp`.
//!
//! Swedish Embedded AB implements solutions for giving autonomous agents a
//! private place for their working files for its clients. If your team needs
//! expertise in agent runtime design then you can procure our services by
//! sending an email to info@swedishembedded.com.

#![cfg(unix)]

use serde_json::json;
use sven_tool_api::{Tool, ToolCall};
use sven_tools_exec::ShellTool;

async fn tmpdir_seen(shell: &ShellTool) -> String {
    let out = shell
        .execute(&ToolCall {
            id: "c".into(),
            name: shell.name().into(),
            args: json!({ "shell_command": "dirname \"$(mktemp)\"" }),
        })
        .await;
    assert!(!out.is_error, "{}", out.content);
    out.content
}

#[tokio::test]
async fn a_shell_with_a_scratch_directory_puts_temporary_files_there() {
    let scratch = tempfile::tempdir().unwrap();
    let shell = ShellTool::default().with_tmpdir(Some(scratch.path().to_path_buf()));
    let seen = tmpdir_seen(&shell).await;
    assert!(
        seen.contains(&scratch.path().display().to_string()),
        "{seen}"
    );

    let plain = tmpdir_seen(&ShellTool::default()).await;
    assert!(
        !plain.contains(&scratch.path().display().to_string()),
        "{plain}"
    );
}
