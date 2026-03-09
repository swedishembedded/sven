// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Inspector slash commands: `/skills`, `/subagents`, `/peers`, `/context`.
//!
//! Each command opens the full-screen [`InspectorOverlay`] for the
//! corresponding view kind.  The actual content is rendered in
//! `App::submit_user_input` when it handles [`ImmediateAction::OpenInspector`].

use crate::{
    commands::{CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand},
    ui::InspectorKind,
};

// ── /skills ───────────────────────────────────────────────────────────────────

/// Open the skills inspector.
pub struct SkillsCommand;

impl SlashCommand for SkillsCommand {
    fn name(&self) -> &str {
        "skills"
    }

    fn description(&self) -> &str {
        "Show all available skills as a browsable tree with paths and metadata."
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
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Skills,
            }),
            ..Default::default()
        }
    }
}

// ── /subagents ────────────────────────────────────────────────────────────────

/// Open the subagents inspector.
pub struct SubagentsCommand;

impl SlashCommand for SubagentsCommand {
    fn name(&self) -> &str {
        "subagents"
    }

    fn description(&self) -> &str {
        "Show all configured subagents with their descriptions, models, and paths."
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
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Subagents,
            }),
            ..Default::default()
        }
    }
}

// ── /peers ────────────────────────────────────────────────────────────────────

/// Open the peers inspector.
pub struct PeersCommand;

impl SlashCommand for PeersCommand {
    fn name(&self) -> &str {
        "peers"
    }

    fn description(&self) -> &str {
        "Show configured subagents and active subprocess buffers \
         (subagents spawned via the task tool)."
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
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Peers,
            }),
            ..Default::default()
        }
    }
}

// ── /context ──────────────────────────────────────────────────────────────────

/// Open the context inspector.
pub struct ContextCommand;

impl SlashCommand for ContextCommand {
    fn name(&self) -> &str {
        "context"
    }

    fn description(&self) -> &str {
        "Show the current agent context: project root, skills/agents counts, \
         and active output buffer handles."
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
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Context,
            }),
            ..Default::default()
        }
    }
}

// ── /tools ────────────────────────────────────────────────────────────────────

/// Open the tools inspector.
pub struct ToolsCommand;

impl SlashCommand for ToolsCommand {
    fn name(&self) -> &str {
        "tools"
    }

    fn description(&self) -> &str {
        "Show all available tools with descriptions and parameter counts. \
         In node-proxy mode, fetches the tool list live from the connected node."
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
            immediate_action: Some(ImmediateAction::OpenInspector {
                kind: InspectorKind::Tools,
            }),
            ..Default::default()
        }
    }
}
