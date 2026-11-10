// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running several coding agents
// in parallel on one repository, with every change integrated or reported,
// for its clients. If your team needs expertise in parallel agent workflows
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The production [`Spawner`]: a task is one `task` call that spawns a
//! writing child with `isolation: "worktree"`. Starting the child, holding it
//! to the parent's authority, the serialized three-way integration and the
//! kept branch of a conflict are all that tool's; this reads its result.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};

use super::graph::TaskSpec;
use super::schedule::{ChildOutcome, Spawner, Status};
use crate::task_tool::isolation::{BRANCH_MARK, CONFLICT_MARK, FILES_HEADER};
use crate::task_tool::TaskTool;

/// The longest child report a graph result carries, in characters.
const REPORT_CHARS: usize = 3000;

/// Runs tasks through `task`. Every child is reported under the id of the
/// `plan_tasks` call, so a trajectory ties each embedded child to it.
pub(super) struct TaskToolSpawner {
    pub(super) task: Arc<TaskTool>,
}

#[async_trait]
impl Spawner for TaskToolSpawner {
    async fn run(&self, call_id: &str, task: &TaskSpec, brief: String) -> ChildOutcome {
        let call = ToolCall {
            id: call_id.to_string(),
            name: "task".into(),
            args: json!({
                "prompt": brief,
                "mode": "agent",
                "isolation": "worktree",
                "description": format!("{}: {}", task.id, task.title),
            }),
        };
        let output = self.task.execute(&call).await;
        let tokens = handle_of(&output.content).and_then(|h| self.task.tokens_used_by(h));
        read_outcome(&output, tokens)
    }
}

fn handle_of(content: &str) -> Option<&str> {
    content
        .lines()
        .find_map(|l| l.strip_prefix("Handle: "))
        .map(str::trim)
}

fn branch_of(content: &str) -> Option<String> {
    let rest = &content[content.find(BRANCH_MARK)? + BRANCH_MARK.len()..];
    Some(rest[..rest.find('\'')?].to_string())
}

fn files_of(content: &str) -> Vec<String> {
    content
        .split_once(FILES_HEADER)
        .map(|(_, list)| {
            list.lines()
                .filter_map(|l| l.strip_prefix("- "))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn bounded(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(REPORT_CHARS) {
        Some((end, _)) => format!("{}... [report cut]", &text[..end]),
        None => text.to_string(),
    }
}

/// What a `task` result says happened to its child.
pub(super) fn read_outcome(output: &ToolOutput, tokens: Option<(u64, u64)>) -> ChildOutcome {
    let content = &output.content;
    let status = if !output.is_error {
        Status::Integrated
    } else if content.contains(CONFLICT_MARK) {
        Status::Conflict
    } else {
        Status::Failed
    };
    let report = if output.is_error {
        content.as_str()
    } else {
        // The child's words, without the tool's own integration footer.
        let words = content
            .split_once("--- Result ---\n")
            .map_or(content.as_str(), |(_, w)| w);
        words.split("\n\nIntegrated").next().unwrap_or(words)
    };
    ChildOutcome {
        status,
        files_changed: if output.is_error {
            Vec::new()
        } else {
            files_of(content)
        },
        branch: branch_of(content),
        report: bounded(report),
        tokens,
    }
}

#[cfg(test)]
#[path = "spawner_tests.rs"]
mod tests;
