// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The reverse of [`crate::StepAssembler`]: a trajectory's steps back into the
//! flat message, turn and conversation shapes that seed an agent's history on
//! resume and feed the legacy readers.

use atif::{ContentSegment, MessageBody, StepOrigin, TraceStep};
use sven_model::Message;

use crate::chat_document::{json_str_to_yaml, TurnRecord};
use crate::compaction_details::ContextCompactionDetails;
use crate::conversation::ConversationRecord;

// ── Reverse: TraceStep ⇄ Message ────────────────────────────────────────────

/// Reverse of the turn assembler: un-merge `Vec<TraceStep>` back into a
/// `Vec<Message>` suitable for seeding an agent's history on resume
/// (equivalent to today's `chat_document::turns_to_messages`).
///
/// - `System`-source steps are skipped entirely.
/// - Steps with `is_copied_context == Some(true)` are skipped — per the
///   spec's SFT-filtering guidance ([`atif::Trajectory::sft_steps`]), repurposed
///   here for the same underlying reason: don't replay content the model
///   didn't originally produce fresh.
/// - An agent step is one model turn and comes back in the order it
///   happened: its non-empty `message` text as a `Role::Assistant` text
///   message, then each of its `tool_calls` as a `ToolCall` message (in array
///   order), then the matching `observation.results` entries as `ToolResult`
///   messages (matched by `tool_call_id == source_call_id`, in call order).
///   Text after results is a step of its own, so none trails the calls.
/// - `reasoning_content` is never message content: it is the
///   [`Message::reasoning`] of the turn's first message, which a provider
///   that takes reasoning sends back.
pub fn steps_to_messages(steps: &[TraceStep]) -> Vec<Message> {
    let mut out = Vec::new();
    for step in steps {
        if step.is_excluded_from_sft() {
            continue;
        }
        match step.source {
            StepOrigin::System => continue,
            StepOrigin::User => {
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(Message::user(text));
                    }
                }
            }
            StepOrigin::Agent => {
                // One model turn: its text, then its calls, then the results
                // that answered them; the reasoning rides on the first.
                let first = out.len();
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(Message::assistant(text));
                    }
                }
                let calls = step.tool_calls.as_deref().unwrap_or_default();
                for call in calls {
                    out.push(Message::tool_call(
                        call.tool_call_id.clone(),
                        call.function_name.clone(),
                        call.arguments.to_string(),
                    ));
                }
                if let Some(first) = out.get_mut(first) {
                    first.reasoning.clone_from(&step.reasoning_content);
                }
                if let Some(observation) = &step.observation {
                    for call in calls {
                        if let Some(entry) = observation.results.iter().find(|r| {
                            r.source_call_id.as_deref() == Some(call.tool_call_id.as_str())
                        }) {
                            let content = entry
                                .content
                                .as_ref()
                                .and_then(message_body_to_text)
                                .unwrap_or_default();
                            out.push(Message::tool_result(call.tool_call_id.clone(), content));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Extract plain text from a [`MessageBody`]: the text form as-is, or (for
/// `Segments`) the concatenation of its text segments, dropping any image
/// segments — a best-effort, documented-lossy fallback since
/// `sven_model::Message` has no way to reference an ATIF file-path image
/// without reading it into a data URL first (out of scope here; see
/// [`StepAssembler::push_message`]'s doc comment for the forward-direction
/// counterpart of this same limitation).
fn message_body_to_text(body: &MessageBody) -> Option<String> {
    if let Some(text) = body.as_text() {
        return Some(text.to_string());
    }
    body.as_segments().map(|segments| {
        segments
            .iter()
            .filter_map(|segment| match segment {
                ContentSegment::Text { text } => Some(text.as_str()),
                ContentSegment::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

// ── Reverse: TraceStep ⇄ TurnRecord (legacy YAML) ───────────────────────────

/// Reverse of [`crate::trace_session::turn_records_to_steps`]: convert `Vec<TraceStep>` back into
/// legacy `Vec<TurnRecord>`, for callers that accumulate ATIF-native steps
/// during a run but still need to write (or update) a YAML `ChatDocument`
/// from them (e.g. `--output-chat` alongside `--trace`).
///
/// - Steps with `is_copied_context == Some(true)` are skipped, matching
///   [`steps_to_messages`]. **Callers that rebuild a trajectory from this
///   view must carry those steps through** (capture them with
///   [`crate::trace_session::copied_context_steps`] and rebuild with
///   [`crate::trace_session::turn_records_to_steps_with_copied_context`] /
///   [`crate::trace_session::conversation_records_to_steps_with_copied_context`]) or an
///   open→save round trip permanently deletes them.
/// - `System`-source steps are skipped, except a context-compaction step
///   (detected via [`ContextCompactionDetails::from_step_extra`]), which
///   becomes `TurnRecord::ContextCompacted`. Any other `System` step has no
///   `TurnRecord` analog and is dropped.
/// - An agent step's `reasoning_content`, if present, becomes a leading
///   `TurnRecord::Thinking`.
/// - `tool_calls` are emitted next (each its own `TurnRecord::ToolCall`, in
///   array order), matched by `tool_call_id` to their `observation.results`
///   entry (each its own `TurnRecord::ToolResult`) — same ordering rule as
///   [`steps_to_messages`]. Finally, non-empty step `message` text becomes a
///   trailing `TurnRecord::User`/`TurnRecord::Assistant` (by `source`).
pub fn steps_to_turn_records(steps: &[TraceStep]) -> Vec<TurnRecord> {
    let mut out = Vec::new();
    for step in steps {
        if step.is_excluded_from_sft() {
            continue;
        }
        match step.source {
            StepOrigin::System => {
                if let Some(extra) = &step.extra {
                    if let Some(details) = ContextCompactionDetails::from_step_extra(extra) {
                        out.push(TurnRecord::ContextCompacted {
                            tokens_before: details.tokens_before,
                            tokens_after: details.tokens_after,
                            strategy: details.strategy,
                            turn: details.turn,
                        });
                    }
                }
            }
            StepOrigin::User => {
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(TurnRecord::User { content: text });
                    }
                }
            }
            StepOrigin::Agent => {
                if let Some(reasoning) = &step.reasoning_content {
                    out.push(TurnRecord::Thinking {
                        content: reasoning.clone(),
                    });
                }
                if let Some(tool_calls) = &step.tool_calls {
                    for call in tool_calls {
                        out.push(TurnRecord::ToolCall {
                            tool_call_id: call.tool_call_id.clone(),
                            name: call.function_name.clone(),
                            arguments: json_str_to_yaml(&call.arguments.to_string()),
                        });
                    }
                    if let Some(observation) = &step.observation {
                        for call in tool_calls {
                            if let Some(entry) = observation.results.iter().find(|r| {
                                r.source_call_id.as_deref() == Some(call.tool_call_id.as_str())
                            }) {
                                let content = entry
                                    .content
                                    .as_ref()
                                    .and_then(message_body_to_text)
                                    .unwrap_or_default();
                                out.push(TurnRecord::ToolResult {
                                    tool_call_id: call.tool_call_id.clone(),
                                    content,
                                });
                            }
                        }
                    }
                }
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(TurnRecord::Assistant { content: text });
                    }
                }
            }
        }
    }
    out
}

/// Reverse of [`crate::trace_session::conversation_records_to_steps`]: convert `Vec<TraceStep>` back
/// into full-fidelity `Vec<ConversationRecord>` (messages, thinking blocks,
/// context-compaction markers), for callers that need to reconstruct a
/// display-oriented event stream from ATIF steps rather than the flat
/// `Message`-only view [`steps_to_messages`] provides.
///
/// Built directly on [`steps_to_turn_records`] + [`crate::chat_document::turns_to_records`]
/// — the same full-fidelity turn list, just re-expressed as `ConversationRecord`
/// instead of the legacy YAML-oriented `TurnRecord`. This keeps exactly one
/// TraceStep-to-turn-shape algorithm in the crate rather than duplicating the
/// step-walking logic for two output types.
pub fn steps_to_conversation_records(steps: &[TraceStep]) -> Vec<ConversationRecord> {
    let turns = steps_to_turn_records(steps);
    crate::chat_document::turns_to_records(&turns)
}
