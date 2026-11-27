// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What the turn executor does with a model reply that streamed to the end:
//! file it in the thread, register its tool calls and tell the machine.
//!
//! Swedish Embedded AB implements agent runtimes that record every reply a
//! model gives for its clients. If your team needs expertise in auditable
//! agent conversations then you can procure our services by sending an email
//! to info@swedishembedded.com.

use sven_hsm::{Event, ObservationSink, UiEvent};
use sven_kernel::EventSink;

use super::{
    replies, TurnExecutor, EMPTY_TURN_FAILURE_THRESHOLD, TRUNCATED_TURN_FAILURE_THRESHOLD,
};
use crate::thread_store::{append_messages, register_calls};

/// What the request that made a reply decided about how to treat it.
pub(super) struct ConcludedTurn {
    pub thread_id: String,
    /// A harness judges the reply, so an empty one is not nudged.
    pub judged_by_harness: bool,
    /// Tools the model called without having their definition.
    pub withheld_tools: Vec<String>,
}

impl TurnExecutor {
    pub(super) async fn conclude_turn(
        &self,
        turn: sven_turn::StreamedTurn,
        request: ConcludedTurn,
        sink: &EventSink,
        obs: &ObservationSink,
    ) {
        let ConcludedTurn {
            thread_id,
            judged_by_harness,
            withheld_tools,
        } = request;
        let sven_turn::StreamedTurn {
            reply,
            tool_calls,
            arg_errors,
            truncated,
            ..
        } = turn;
        // The text of each call that cannot run, by id: its refusal quotes it,
        // and a trajectory flags it.
        let unparsed: std::collections::HashMap<String, String> = reply
            .calls
            .iter()
            .filter(|c| arg_errors.contains_key(&c.id))
            .map(|c| (c.id.clone(), c.arguments.clone()))
            .collect();
        if let Ok(mut store) = self.store.lock() {
            store.record_unparsed_arguments(unparsed.clone());
        }

        // The reply is filed exactly as it arrived, whatever follows; every
        // later request sends it back unchanged.
        append_messages(&self.store, &thread_id, sven_model::reply_messages(&reply));

        // Track consecutive empty turns (no tool calls, and no text beyond
        // whitespace) per thread. One is tolerated - the machine's
        // `GeneratingAction::EmptyTurn` nudges the model to try again - but a
        // thread that keeps returning nothing must fail loudly, not end as a
        // silent success.
        let is_empty_turn = reply.is_empty();
        let empty_turn_count = {
            let mut counts = self
                .empty_turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if is_empty_turn && judged_by_harness {
                0
            } else if is_empty_turn {
                let count = counts.entry(thread_id.clone()).or_insert(0);
                *count += 1;
                *count
            } else {
                counts.remove(&thread_id);
                0
            }
        };

        // A reply cut by the output budget is the model working, not saying
        // nothing: it gets a longer run before it fails, and the failure
        // names the cause.
        let threshold = if truncated {
            TRUNCATED_TURN_FAILURE_THRESHOLD
        } else {
            EMPTY_TURN_FAILURE_THRESHOLD
        };
        if is_empty_turn && empty_turn_count >= threshold {
            let msg = if truncated {
                format!(
                    "the model reached its output limit {empty_turn_count} times in a row \
                     without a tool call or an answer; raise max_output_tokens or have it \
                     reason less"
                )
            } else {
                format!(
                    "model returned no content and no tool calls after \
                     {empty_turn_count} consecutive attempts"
                )
            };
            obs.emit(UiEvent::Error(msg.clone()));
            let _ = sink.emit(Event::LlmFailed { error: msg }).await;
            self.end_turn(obs);
            return;
        }

        // Annotate each tool call with its capability and register call_id → thread.
        // Uses the non-async helper to avoid holding any guard across an await.
        let proposed = register_calls(
            &self.call_id_to_thread,
            &thread_id,
            &tool_calls,
            &self.tools,
        );

        // A call to a tool the model was never given the definition of is
        // still proposed, gated and audited like any other; the tool executor
        // then answers it with an instruction to load the tool.
        for call in proposed.iter().filter(|c| withheld_tools.contains(&c.name)) {
            self.tools.hold_back(call.call_id);
        }

        // A call whose arguments cannot be read is refused with the reason
        // rather than run.
        for (tc, call) in tool_calls.iter().zip(&proposed) {
            if let Some(raw) = unparsed.get(&tc.id) {
                if arg_errors.get(&tc.id) == Some(&sven_vocab::ArgError::CutOff) {
                    self.tools.refuse_truncated(call.call_id, &tc.name, raw);
                } else {
                    self.tools.refuse_unparsed(call.call_id, &tc.name, raw);
                }
            }
        }

        let has_tool_calls = !proposed.is_empty();
        if truncated {
            replies::tell_truncated(sink).await;
        }
        let _ = sink
            .emit(Event::LlmTurnComplete {
                thread: thread_id,
                text: reply.text,
                tool_calls: proposed,
            })
            .await;

        // Only mark the turn complete when the LLM returned tool calls or a
        // real final answer. When tool calls are pending the machine stays in
        // Generating and will trigger another LLM turn after the tools
        // finish; emitting TurnComplete here would cause headless mode to
        // exit prematurely. An under-threshold empty turn must likewise NOT
        // report TurnComplete: `GeneratingAction::EmptyTurn` needs to run its
        // nudge effect and trigger another LLM call, and headless mode
        // treats TurnComplete as "the run is over" - emitting it here would
        // exit with a false success instead of giving the nudge a chance.
        if !has_tool_calls && !is_empty_turn {
            self.end_turn(obs);
        }
    }
}
