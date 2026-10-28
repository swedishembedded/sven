// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Slash commands typed at a session this host runs for a server.
//!
//! The same registry the terminal uses, cut to what works remotely
//! ([`CommandRegistry::for_remote`]): a project's own command becomes the
//! message it stands for, `/mode` switches the session's mode, and anything
//! else that only looks like a command (`/etc/hosts?`) is an ordinary message.

use std::path::Path;

use sven_control::SlashCommandInfo;
use sven_frontend::commands::{try_dispatch, CommandRegistry, ImmediateAction};
use sven_vocab::AgentMode;

/// What a line typed at a session means.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Typed {
    /// An ordinary message, sent as it is.
    Message(String),
    /// A command that stands for a message: send this instead.
    Expanded(String),
    /// `/mode <mode>`: the session's mode from the next turn.
    Mode(AgentMode),
    /// `/clear`: the session starts over with no conversation.
    Clear,
    /// A command used wrongly: say this back, send nothing.
    Usage(String),
}

pub(super) fn interpret(text: &str, project_root: &Path) -> Typed {
    if !text.starts_with('/') {
        return Typed::Message(text.to_string());
    }
    let registry = CommandRegistry::for_remote(Some(project_root));
    let Some((name, result)) = try_dispatch(text, &registry) else {
        return Typed::Message(text.to_string());
    };
    if let Some(mode) = result.mode_override {
        return Typed::Mode(mode);
    }
    if matches!(
        result.immediate_action,
        Some(ImmediateAction::ClearChat | ImmediateAction::NewConversation)
    ) {
        return Typed::Clear;
    }
    if let Some(message) = result.message_to_send {
        return Typed::Expanded(message);
    }
    if name == "mode" {
        return Typed::Usage(
            "Use /mode with one of: agent, plan, research, chat, sdlc.".to_string(),
        );
    }
    Typed::Usage(format!("/{name} cannot be run here."))
}

/// The commands a session here offers, by name.
pub(super) fn available(project_root: &Path) -> Vec<SlashCommandInfo> {
    let registry = CommandRegistry::for_remote(Some(project_root));
    let mut commands: Vec<SlashCommandInfo> = registry
        .iter()
        .map(|command| SlashCommandInfo {
            name: command.name().to_string(),
            description: command.description().to_string(),
        })
        .collect();
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_command() -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        let commands = project.path().join(".sven/commands");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::write(
            commands.join("review-pr.md"),
            "---\ndescription: Review a pull request\n---\nReview the pull request carefully.\n",
        )
        .unwrap();
        project
    }

    #[test]
    fn a_projects_own_command_stands_for_its_text_and_takes_the_rest_as_the_task() {
        let project = project_with_command();
        assert_eq!(
            interpret("/review-pr 42", project.path()),
            Typed::Expanded("Review the pull request carefully.\n\nTask: 42".to_string())
        );
    }

    #[test]
    fn mode_switches_the_session_and_says_how_when_used_wrongly() {
        let project = project_with_command();
        assert_eq!(
            interpret("/mode plan", project.path()),
            Typed::Mode(AgentMode::Plan)
        );
        assert!(matches!(
            interpret("/mode", project.path()),
            Typed::Usage(_)
        ));
    }

    #[test]
    fn a_line_that_only_looks_like_a_command_is_a_message() {
        let project = project_with_command();
        for line in ["hello", "/etc/hosts is empty?", "/quit"] {
            assert_eq!(
                interpret(line, project.path()),
                Typed::Message(line.to_string())
            );
        }
    }

    #[test]
    fn lists_what_a_session_here_can_run() {
        let project = project_with_command();
        let names: Vec<String> = available(project.path())
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"review-pr".to_string()));
        assert!(names.contains(&"mode".to_string()));
        assert!(!names.contains(&"quit".to_string()));
    }
}
