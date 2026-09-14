// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/learn` - trigger the `knowledge-extract` subagent persona (see
//! `.sven/agents/knowledge-extract.md`) against the current session's own
//! trajectory or an arbitrary document.
//!
//! This is a thin macro: it does not spawn anything itself, it queues a
//! deterministic instruction for the running agent to act on via the `task`
//! tool's persona dispatch (`sven_bootstrap::task_tool::resolve_mode_and_prompt`).
//! Keeping the dispatch decision inside the ordinary LLM turn - rather than
//! adding a new `Effect` - means no HSM/kernel changes are needed for this
//! command to work.

use crate::{CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand};

/// Name of the subagent persona this command delegates to (must match the
/// `name:` frontmatter field in `.sven/agents/knowledge-extract.md`).
const PERSONA: &str = "knowledge-extract";

pub struct LearnCommand;

impl SlashCommand for LearnCommand {
    fn name(&self) -> &str {
        "learn"
    }

    fn description(&self) -> &str {
        "Extract curated, high-signal training data (usage: /learn session | /learn document <path>)"
    }

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }
        let items = vec![
            CompletionItem::with_desc(
                "session",
                "session",
                "Learn from the current session's trajectory",
            ),
            CompletionItem::with_desc(
                "document",
                "document",
                "Learn from a document (provide a path next)",
            ),
        ];
        crate::completion::filter_and_rank(items, partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let message = match args.split_first() {
            Some((first, _rest)) if first == "session" => Some(format!(
                "Use the `task` tool with mode=\"{PERSONA}\" to learn everything you can from \
                 the current session. Have the sub-agent locate the most recently modified \
                 `*.atif.json` trajectory file under `.sven/logs/` and pass that path in its \
                 prompt, with an instruction to append any newly curated examples to the \
                 successful-trajectory store at `.sven/knowledge/curated/`."
            )),
            Some((first, rest)) if first == "document" && !rest.is_empty() => Some(format!(
                "Use the `task` tool with mode=\"{PERSONA}\" to learn everything you can from \
                 the document at `{}`. The sub-agent may run small experiments (write and \
                 execute short scripts or commands) to verify what it learns before curating \
                 it, and should append the result to the successful-trajectory store at \
                 `.sven/knowledge/curated/`.",
                rest.join(" ")
            )),
            _ => None,
        };

        match message {
            Some(message_to_send) => CommandResult {
                message_to_send: Some(message_to_send),
                ..Default::default()
            },
            None => CommandResult {
                immediate_action: Some(ImmediateAction::Notice {
                    text: "Usage: /learn session | /learn document <path>".to_string(),
                }),
                ..Default::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_form_mentions_persona_and_logs_dir() {
        let result = LearnCommand.execute(vec!["session".into()]);
        let msg = result.message_to_send.expect("expected a queued message");
        assert!(msg.contains(PERSONA));
        assert!(msg.contains(".sven/logs/"));
        assert!(msg.contains(".sven/knowledge/curated/"));
    }

    #[test]
    fn document_form_includes_path_and_allows_experiments() {
        let result = LearnCommand.execute(vec!["document".into(), "notes.md".into()]);
        let msg = result.message_to_send.expect("expected a queued message");
        assert!(msg.contains(PERSONA));
        assert!(msg.contains("notes.md"));
        assert!(msg.contains("experiments"));
    }

    #[test]
    fn document_form_with_spaced_path_joins_args() {
        let result = LearnCommand.execute(vec![
            "document".into(),
            "my".into(),
            "notes.md".into(),
        ]);
        let msg = result.message_to_send.expect("expected a queued message");
        assert!(msg.contains("my notes.md"));
    }

    #[test]
    fn document_form_without_path_is_usage_notice() {
        let result = LearnCommand.execute(vec!["document".into()]);
        assert!(result.message_to_send.is_none());
        match result.immediate_action {
            Some(ImmediateAction::Notice { text }) => assert!(text.starts_with("Usage:")),
            other => panic!("expected usage notice, got {other:?}"),
        }
    }

    #[test]
    fn no_args_is_usage_notice() {
        let result = LearnCommand.execute(vec![]);
        assert!(result.message_to_send.is_none());
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::Notice { .. })
        ));
    }

    #[test]
    fn unknown_subcommand_is_usage_notice() {
        let result = LearnCommand.execute(vec!["bogus".into()]);
        assert!(result.message_to_send.is_none());
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::Notice { .. })
        ));
    }
}
