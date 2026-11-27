// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Per-turn announcement of the skills, subagents and knowledge documents
//! whose activation condition holds.

use sven_hsm::Event;
use sven_kernel::EventSink;
use sven_model::{Message, Role};
use sven_turn::instructions::Activator;
use sven_vocab::TurnRequest;

/// Records the instructions this turn newly activates and returns their
/// announcement, which the caller appends to the conversation as a note.
///
/// The task is the latest user message of the thread: a file it names
/// activates what covers that file. The kernel keeps the set, so an
/// instruction is announced once, when it becomes active.
pub(super) async fn announce(
    activator: Option<&Activator>,
    req: &TurnRequest,
    messages: &[Message],
    sink: &EventSink,
) -> Option<String> {
    let activator = activator?;
    let task = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .and_then(Message::as_text)
        .unwrap_or("");
    let plan = activator.plan_turn(&req.active_instructions, &req.state, task);
    if !plan.activate.is_empty() {
        let _ = sink
            .emit(Event::InstructionsActivated { ids: plan.activate })
            .await;
    }
    plan.block
}

/// Files `note` in `thread` as a user-role message of its own and announces
/// it: the system prompt stays as the thread's earlier requests sent it.
pub(super) fn file_note(
    store: &std::sync::Arc<std::sync::Mutex<crate::thread_store::ThreadStore>>,
    thread: &str,
    note: String,
    obs: &sven_hsm::ObservationSink,
) -> Message {
    let message = Message::user(&note);
    crate::thread_store::append_messages(store, thread, vec![message.clone()]);
    obs.emit(sven_hsm::UiEvent::NoteFiled(note));
    message
}
