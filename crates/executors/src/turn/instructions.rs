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

/// Records the instructions this turn newly activates and appends the
/// announcement of every active one to the request's uncached suffix.
///
/// The task is the latest user message of the thread: a file it names
/// activates what covers that file. The kernel keeps the set, so an
/// instruction announced once is announced on every later turn.
pub(super) async fn announce(
    activator: Option<&Activator>,
    req: &mut TurnRequest,
    messages: &[Message],
    sink: &EventSink,
) {
    let Some(activator) = activator else {
        return;
    };
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
    if let Some(block) = plan.block {
        req.dynamic_suffix = Some(match req.dynamic_suffix.take() {
            Some(existing) if !existing.trim().is_empty() => format!("{existing}\n\n{block}"),
            _ => block,
        });
    }
}
