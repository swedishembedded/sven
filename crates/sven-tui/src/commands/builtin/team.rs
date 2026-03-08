// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Team collaboration slash commands:
//!   - `/approve [task_id]` — approve a pending plan submitted by a teammate
//!   - `/reject [task_id] [reason]` — reject a pending plan with feedback

use crate::commands::{
    CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

// ── /approve ──────────────────────────────────────────────────────────────────

/// Approve the most recent pending plan, or a specific task by ID.
pub struct ApproveCommand;

impl SlashCommand for ApproveCommand {
    fn name(&self) -> &str {
        "approve"
    }

    fn description(&self) -> &str {
        "Approve a teammate's pending plan. Pass an optional task_id to approve \
         a specific plan; without an argument, approves the latest pending plan."
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let task_id = args.first().cloned().unwrap_or_default();
        CommandResult {
            immediate_action: Some(ImmediateAction::ApprovePlan { task_id }),
            ..Default::default()
        }
    }
}

// ── /reject ───────────────────────────────────────────────────────────────────

/// Reject a pending plan with feedback.
pub struct RejectCommand;

impl SlashCommand for RejectCommand {
    fn name(&self) -> &str {
        "reject"
    }

    fn description(&self) -> &str {
        "Reject a teammate's pending plan. Pass an optional task_id as the first \
         argument; remaining arguments are the rejection feedback."
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let mut it = args.into_iter();
        let task_id = it.next().unwrap_or_default();
        let feedback = it.collect::<Vec<_>>().join(" ");
        CommandResult {
            immediate_action: Some(ImmediateAction::RejectPlan { task_id, feedback }),
            ..Default::default()
        }
    }
}

// ── /agents ───────────────────────────────────────────────────────────────────

/// Open the agent/team picker overlay.
pub struct AgentsCommand;

impl SlashCommand for AgentsCommand {
    fn name(&self) -> &str {
        "agents"
    }

    fn description(&self) -> &str {
        "Show the team members overlay (also Ctrl+a). \
         Navigate with arrow keys, Enter to switch to that agent's view."
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::OpenTeamPicker),
            ..Default::default()
        }
    }
}

// ── /architect ────────────────────────────────────────────────────────────────

/// Switch into architect/editor two-model mode.
pub struct ArchitectCommand;

impl SlashCommand for ArchitectCommand {
    fn name(&self) -> &str {
        "architect"
    }

    fn description(&self) -> &str {
        "Start architect/editor mode: this agent acts as the architect, planning and \
         specifying changes, while a separate editor agent implements them."
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let msg = if args.is_empty() {
            "Enter architect mode: I will plan and specify changes, \
             delegating implementation to an editor agent. \
             What would you like me to design?"
                .to_string()
        } else {
            args.join(" ")
        };
        CommandResult {
            message_to_send: Some(msg),
            ..Default::default()
        }
    }
}

// ── /tasks ────────────────────────────────────────────────────────────────────

/// Show the team task list.
pub struct TasksCommand;

impl SlashCommand for TasksCommand {
    fn name(&self) -> &str {
        "tasks"
    }

    fn description(&self) -> &str {
        "Show the current team task list (also Ctrl+t)."
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, _args: Vec<String>) -> CommandResult {
        CommandResult {
            immediate_action: Some(ImmediateAction::ToggleTaskList),
            ..Default::default()
        }
    }
}
