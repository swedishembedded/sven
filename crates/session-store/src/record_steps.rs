// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.
//! One conversation record as ATIF steps.
//!
//! An agent's run, a harness's plan thread and a child run's thread are all
//! the same thing to a trajectory: messages in order, each model request's
//! account at the message its response ended on, the compactions where they
//! fired, and what the permission gate and memory tools recorded about the
//! calls. This is the one place that turns them into steps, so every export
//! of a conversation says the same things the same way.

use atif::{CallTiming, GateDecision, MemoryRetrieval, TraceStep};
use sven_model::Message;
use sven_vocab::{CompactionRecord, RequestRecord};

use crate::trace_session::StepAssembler;

/// What is known about one conversation.
pub struct ConversationFacts<'a> {
    /// Every message, in the order it happened.
    pub messages: &'a [Message],
    /// Compactions, each with the number of `messages` before it fired.
    pub compactions: &'a [(usize, CompactionRecord)],
    /// Requests, each with the number of `messages` up to and including its
    /// response.
    pub requests: &'a [(usize, RequestRecord)],
    /// What the permission gate decided about calls.
    pub gate_decisions: &'a [GateDecision],
    /// What memory retrievals returned.
    pub memory_retrievals: &'a [MemoryRetrieval],
    /// How long tool calls ran.
    pub call_timings: &'a [CallTiming],
    /// What the model reasoned before the assistant message at each position
    /// in `messages`. Becomes the step's `reasoning_content`.
    pub reasonings: &'a [(usize, String)],
    /// Where user turns came from, each with its position in `messages`.
    pub origins: &'a [(usize, String)],
    /// File each system message as a `system` step. Off for a conversation
    /// whose system prompt is the product's own; on for one whose system
    /// message a harness chose.
    pub system_steps: bool,
}

/// The steps of `facts`. `as_sent` gives a call's raw argument text when the
/// model's arguments were not valid JSON, so the step keeps and flags it.
pub fn conversation_steps(
    facts: &ConversationFacts<'_>,
    as_sent: &dyn Fn(&Message) -> Message,
) -> Vec<TraceStep> {
    let mut assembler = StepAssembler::new();
    let marks = |assembler: &mut StepAssembler, position: usize| {
        for (_, c) in facts.compactions.iter().filter(|(at, _)| *at == position) {
            assembler.push_context_compacted(
                c.tokens_before,
                c.tokens_after,
                Some(&c.strategy.to_string()),
                Some(c.turn),
            );
        }
    };
    let requests = |assembler: &mut StepAssembler, position: usize| {
        for (_, request) in facts.requests.iter().filter(|(at, _)| *at == position) {
            assembler.push_request(request);
        }
    };
    marks(&mut assembler, 0);
    for decision in facts.gate_decisions {
        assembler.push_gate_decision(decision.clone());
    }
    for retrieval in facts.memory_retrievals {
        assembler.push_memory_retrieval(retrieval.clone());
    }
    for timing in facts.call_timings {
        assembler.push_call_timing(timing.clone());
    }
    requests(&mut assembler, 0);
    for (position, message) in facts.messages.iter().enumerate() {
        if position > 0 {
            marks(&mut assembler, position);
        }
        for (_, reasoning) in facts.reasonings.iter().filter(|(at, _)| *at == position) {
            assembler.push_reasoning(reasoning);
        }
        match (&message.role, message.as_text()) {
            (sven_model::Role::System, Some(text)) if facts.system_steps => {
                assembler.push_system(text);
            }
            _ => assembler.push_message(&as_sent(message)),
        }
        if let Some((_, origin)) = facts.origins.iter().find(|(at, _)| *at == position) {
            assembler.label_last_step(origin);
        }
        requests(&mut assembler, position + 1);
    }
    // A compaction after the last message (the run ended right after it).
    if !facts.messages.is_empty() {
        marks(&mut assembler, facts.messages.len());
    }
    assembler.finish()
}
