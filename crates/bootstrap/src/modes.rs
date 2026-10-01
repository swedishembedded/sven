// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The modes this build can run, and the tools their machines call by name.
//!
//! `sven-machines` holds only pure transition functions and cannot know which
//! tools a build links, so its [`ModeRegistry::default_registry`] carries just
//! the machines that need no particular tool. A machine that drives one tool -
//! `ui-test` and the `android` tool - is registered here, by the assembly that
//! compiles the tool in, and a session in that mode is given the tool. A build
//! without the tool therefore neither lists the mode nor runs it.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in assembling agent frameworks for constrained
//! targets then you can procure our services by sending an email to
//! info@swedishembedded.com.

use sven_machines::ModeRegistry;

/// The mode that drives an Android device step by step (`android` feature).
pub const UI_TEST_MODE: &str = "ui-test";

/// Every mode this build can run: the built-in machines, plus each machine
/// whose tools are compiled in.
#[must_use]
pub fn mode_registry() -> ModeRegistry {
    #[allow(unused_mut)]
    let mut registry = ModeRegistry::default_registry();
    #[cfg(feature = "android")]
    registry.register(
        UI_TEST_MODE,
        Box::new(|| -> Box<dyn sven_hsm::submachine::ErasedMachine> {
            Box::new(sven_hsm::dispatch::Hsm::new(
                sven_machines::UiTestMachine::new(),
            ))
        }),
    );
    registry
}

/// Adds the tools `mode`'s machine calls by name that `registry` does not
/// already hold, so a session in that mode can run whatever tool set it was
/// otherwise given. A question the machine asks goes where the session's
/// questions go: `question_tx`, else parked when `park_questions`, else the
/// no-user answer.
#[cfg(feature = "android")]
pub(crate) fn register_mode_tools(
    mode: &str,
    registry: &mut sven_tool_registry::ToolRegistry,
    question_tx: Option<tokio::sync::mpsc::Sender<crate::QuestionRequest>>,
    park_questions: bool,
) {
    use crate::context::Questions;

    if mode != UI_TEST_MODE {
        return;
    }
    if registry.get("android").is_none() {
        registry.register(sven_tools_android::AndroidTool::default());
    }
    if registry.get("ask_question").is_none() {
        let questions = match question_tx {
            Some(tx) => Questions::Answered(tx),
            None if park_questions => Questions::Parked,
            None => Questions::NoUser,
        };
        crate::registry::register_ask_question(registry, questions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_test_is_a_mode_exactly_when_its_tool_is_compiled_in() {
        assert_eq!(
            mode_registry().get(UI_TEST_MODE).is_some(),
            cfg!(feature = "android")
        );
        for mode in ["agent", "chat", "sdlc", "verified-task", "predict"] {
            assert!(mode_registry().get(mode).is_some(), "{mode}");
        }
    }

    /// A `ui-test` session gets the tools its machine calls, whatever tool
    /// set it was otherwise given; other modes get nothing extra.
    #[cfg(feature = "android")]
    #[test]
    fn a_ui_test_session_is_given_the_tools_its_machine_calls() {
        use sven_tool_registry::ToolRegistry;

        let mut registry = ToolRegistry::new();
        register_mode_tools(UI_TEST_MODE, &mut registry, None, false);
        assert!(registry.get("android").is_some());
        assert!(registry.get("ask_question").is_some());

        let mut other = ToolRegistry::new();
        register_mode_tools("agent", &mut other, None, false);
        assert!(other.get("android").is_none());
        assert!(other.get("ask_question").is_none());
    }
}
