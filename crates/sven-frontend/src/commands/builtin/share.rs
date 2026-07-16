// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/share` command - share this local session so a remote consultant can
//! steer it ("local brain, remote steer").
//!
//! The steering bridge itself lives in the `sven` binary and reuses
//! `sven-node`'s `ControlService` — it cannot be reached from `sven-frontend`,
//! which deliberately does not depend on `sven-node` (the layering rule in
//! AGENTS.md). So the interactive `/share` command emits a
//! [`ImmediateAction::ShareInstructions`] handoff: the frontend shows the exact
//! `sven share` invocation to run against the control plane, which starts the
//! bridge for this workspace. The fully-scriptable path is `sven share`.

use crate::commands::{
    CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

pub struct ShareCommand;

impl SlashCommand for ShareCommand {
    fn name(&self) -> &str {
        "share"
    }

    fn description(&self) -> &str {
        "Share this local session so a remote consultant can steer it"
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
        // An optional argument becomes the human-readable share title.
        let title = if args.is_empty() {
            "shared sven session".to_string()
        } else {
            args.join(" ")
        };
        let text = format!(
            "Share this workspace so a consultant can steer it — run in another terminal:\n\
             \n    sven -c <config> share --url <broker-url> \\\n      \
             --token \"$(cat tenant.token)\" --tenant-id <tenant> \\\n      \
             --title {title:?}\n\
             \nThe consultant then attaches with:\n\
             \n    sven cloud session attach --url <broker-url> \\\n      \
             --share-id <printed-id> --token \"$(cat operator.token)\" --prompt \"…\"\n\
             \nTools run locally; credentials never leave your machine."
        );
        CommandResult {
            immediate_action: Some(ImmediateAction::ShareInstructions { text }),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instructions(result: CommandResult) -> String {
        match result.immediate_action {
            Some(ImmediateAction::ShareInstructions { text }) => text,
            other => panic!("expected ShareInstructions, got {other:?}"),
        }
    }

    #[test]
    fn execute_emits_share_instructions() {
        let text = instructions(ShareCommand.execute(vec![]));
        assert!(text.contains("share --url"), "should show the share command: {text}");
        assert!(text.contains("sven cloud session attach"));
    }

    #[test]
    fn execute_uses_the_argument_as_the_title() {
        let text = instructions(ShareCommand.execute(vec!["debug".into(), "prod".into()]));
        assert!(text.contains("debug prod"), "title should be threaded in: {text}");
    }
}
