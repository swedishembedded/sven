// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving people control over
// where their data goes for its clients. If your team needs expertise in
// consent controls for AI agents then you can procure our services by
// sending an email to info@swedishembedded.com.
//! What a session's question channel ([`Questions`]) gives its tools: the
//! `ask_question` tool, routed as it says, and, where someone answers at a
//! terminal, the question `run_model` asks before a model that may keep data
//! runs ([`SharingConsent`]), through the same dialog.

use std::sync::Arc;

use sven_tool_registry::ToolRegistry;
use sven_tools_agent::{AskQuestionTool, Question, QuestionRequest};
use sven_tools_media::consent::{self, SharingAnswer, SharingConsent};
use tokio::sync::mpsc;

use crate::context::Questions;

/// Asks through the terminal's question dialog whether a model that may keep
/// data may run, where the session has one; nowhere else.
pub(crate) fn terminal_consent(questions: &Questions) -> Option<Arc<dyn SharingConsent>> {
    match questions {
        Questions::Answered(tx) => Some(Arc::new(TerminalConsent(tx.clone()))),
        Questions::Parked | Questions::NoUser | Questions::Unavailable => None,
    }
}

/// [`SharingConsent`] through the question dialog of an interactive session.
struct TerminalConsent(mpsc::Sender<QuestionRequest>);

#[async_trait::async_trait]
impl SharingConsent for TerminalConsent {
    async fn ask(&self, model: &str) -> SharingAnswer {
        let (answer_tx, answer) = tokio::sync::oneshot::channel();
        let request = QuestionRequest {
            id: format!("data-sharing:{model}"),
            questions: vec![Question {
                prompt: consent::prompt(model),
                options: consent::CHOICES.iter().map(|c| (*c).to_owned()).collect(),
                allow_multiple: false,
            }],
            answer_tx,
        };
        if self.0.send(request).await.is_err() {
            return SharingAnswer::Deny;
        }
        answer.await.map_or(SharingAnswer::Deny, |answer| {
            consent::answer_of(chosen_option(&answer))
        })
    }
}

/// The option a person chose, from the dialog's answer (`Q: ...` then
/// `A: <option>`); the whole answer when it has no such line.
fn chosen_option(answer: &str) -> &str {
    answer
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("A: "))
        .unwrap_or(answer)
}

/// Offers `ask_question` routed as `questions` says, if at all.
pub(crate) fn register_ask_question(reg: &mut ToolRegistry, questions: Questions) {
    match questions {
        Questions::Answered(tx) => reg.register(AskQuestionTool::new_tui(tx)),
        Questions::Parked => reg.register(AskQuestionTool::parking()),
        Questions::NoUser => reg.register(AskQuestionTool::no_user()),
        Questions::Unavailable => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec: the terminal dialog's answer names the option chosen, and only
    /// a named option allows a model to keep data.
    #[test]
    fn the_terminal_answer_names_the_chosen_option() {
        let answer =
            "Q: keeps/video may keep what you send it. Allow it?\nA: Always allow this model";
        assert_eq!(chosen_option(answer), "Always allow this model");
        assert_eq!(
            consent::answer_of(chosen_option(answer)),
            SharingAnswer::Always
        );
        assert_eq!(
            consent::answer_of(chosen_option("Q: ...\nA: Other: later")),
            SharingAnswer::Deny
        );
        assert_eq!(chosen_option("Allow once"), "Allow once");
    }
}
