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
use sven_model::{FunctionCall, Message, MessageContent, Role};

use super::{replies, TurnExecutor, EMPTY_TURN_FAILURE_THRESHOLD};
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
        let unparsed = turn.unparsed_arguments.clone();
        if let Ok(mut store) = self.store.lock() {
            store.record_unparsed_arguments(turn.unparsed_arguments);
        }
        let (text, reasoning, tool_calls, truncated) =
            (turn.text, turn.reasoning, turn.tool_calls, turn.truncated);

        // Track consecutive empty turns (no tool calls, and no text beyond
        // whitespace) per thread. One is tolerated - the machine's
        // `GeneratingAction::EmptyTurn` nudges the model to try again - but a
        // thread that keeps returning nothing must fail loudly, not end as a
        // silent success.
        let is_empty_turn = text.trim().is_empty() && tool_calls.is_empty();
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

        if is_empty_turn && empty_turn_count >= EMPTY_TURN_FAILURE_THRESHOLD {
            let msg = format!(
                "model returned no content and no tool calls after \
                 {empty_turn_count} consecutive attempts"
            );
            obs.emit(UiEvent::Error(msg.clone()));
            let _ = sink.emit(Event::LlmFailed { error: msg }).await;
            obs.emit(UiEvent::TurnComplete);
            return;
        }

        // Append the assistant turn to the thread (append-only, cache-safe).
        // Build the messages list first, then call the non-async helper.
        {
            if let Ok(mut store) = self.store.lock() {
                store.record_reasoning(&thread_id, reasoning);
            }
            let mut msgs_to_append = Vec::new();
            if !text.is_empty() {
                msgs_to_append.push(Message::assistant(&text));
            }
            for tc in &tool_calls {
                msgs_to_append.push(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                });
            }
            append_messages(&self.store, &thread_id, msgs_to_append);
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

        // A call whose arguments the model garbled beyond repair carries `{}`;
        // it is refused with the reason rather than run on those arguments.
        for (tc, call) in tool_calls.iter().zip(&proposed) {
            if let Some(raw) = unparsed.get(&tc.id) {
                self.tools.refuse_unparsed(call.call_id, &tc.name, raw);
            }
        }

        let has_tool_calls = !proposed.is_empty();
        if truncated {
            replies::tell_truncated(sink).await;
        }
        let _ = sink
            .emit(Event::LlmTurnComplete {
                thread: thread_id,
                text,
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
            obs.emit(UiEvent::TurnComplete);
        }
    }
}
