// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents with complete,
// structured traces of everything a kernel reports. If your team needs
// expertise in agent observability, you can procure our services by sending
// an email to info@swedishembedded.com.

//! The kernel's event stream, mapped into the run's evidence: every session
//! event becomes one trace record, and the tallies the structured outcome
//! reads are fed as a side effect of that mapping - so an event the mapper
//! does not understand cannot silently vanish from the evidence.

use crate::outcome::Usage;
use crate::trace::Trace;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use sven_sdk::SessionEvent;

/// Bookkeeping the event collector feeds, and the outcome reads. Atomics
/// because the collector runs on another task; each tally cell is written by
/// exactly one kind of event, so `Relaxed` suffices.
#[derive(Default)]
pub(crate) struct Tally {
    tool_calls: AtomicU64,
    failed_tool_calls: AtomicU64,
    compactions: AtomicU64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    /// Tokens served from / written to the provider's prompt cache. The
    /// `input_tokens` cell is fresh-only (the providers report it that way),
    /// so these two are the rest of what was actually processed.
    cache_read_tokens: AtomicU64,
    cache_write_tokens: AtomicU64,
    /// Provider-reported cost, accumulated in units of 1e-5 USD. The
    /// outcome's cost stays `None` unless some report carried a price:
    /// unmeasured is not free.
    cost_e5: AtomicU64,
    saw_cost: AtomicU64,
    tool_failures: Mutex<Vec<String>>,
    mutated_paths: Mutex<Vec<String>>,
    asked_questions: AtomicU64,
}

impl Tally {
    pub(crate) fn usage(&self, duration_secs: u64) -> Usage {
        Usage {
            tool_calls: self.tool_calls.load(Ordering::Relaxed),
            failed_tool_calls: self.failed_tool_calls.load(Ordering::Relaxed),
            compactions: self.compactions.load(Ordering::Relaxed),
            input_tokens: self.input_tokens.load(Ordering::Relaxed),
            output_tokens: self.output_tokens.load(Ordering::Relaxed),
            cache_read_tokens: self.cache_read_tokens.load(Ordering::Relaxed),
            cache_write_tokens: self.cache_write_tokens.load(Ordering::Relaxed),
            cost_usd: if self.saw_cost.load(Ordering::Relaxed) > 0 {
                Some(self.cost_e5.load(Ordering::Relaxed) as f64 / 100_000.0)
            } else {
                None
            },
            duration_secs,
        }
    }

    pub(crate) fn asked_questions(&self) -> u64 {
        self.asked_questions.load(Ordering::Relaxed)
    }

    pub(crate) fn mutated_paths(&self) -> Vec<String> {
        self.mutated_paths.lock().unwrap().clone()
    }

    pub(crate) fn tool_failures(&self) -> Vec<String> {
        self.tool_failures.lock().unwrap().clone()
    }
}

/// Consumes the kernel's event stream into the trace, tallying what the
/// outcome needs. Runs until the channel closes - which happens when the
/// agent is dropped - so nothing the kernel reported is left unwritten.
pub(crate) async fn collect(
    mut stream: tokio::sync::broadcast::Receiver<SessionEvent>,
    trace: Arc<Trace>,
    tally: Arc<Tally>,
) {
    while let Ok(event) = stream.recv().await {
        let kind = kind_of(&event);
        let mut payload = payload_of(event, &tally);
        let _ = trace.event(kind, &mut payload);
    }
}

fn kind_of(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::TextComplete(_) => "text",
        SessionEvent::ThinkingComplete(_) => "thinking",
        SessionEvent::ToolCallStarted(_) => "tool_started",
        SessionEvent::ToolCallFinished { .. } => "tool_finished",
        SessionEvent::ContextCompacted { .. } => "compacted",
        SessionEvent::TokenUsage { .. } => "usage",
        SessionEvent::TurnComplete => "turn_complete",
        SessionEvent::Aborted { .. } => "aborted",
        SessionEvent::Error(_) => "error",
        SessionEvent::TodoUpdate(_) => "plan",
        SessionEvent::Question { .. } => "question",
        SessionEvent::ModelChanged(_) => "model_changed",
        _ => "other",
    }
}

fn payload_of(event: SessionEvent, tally: &Tally) -> serde_json::Value {
    use Ordering::Relaxed;
    match event {
        SessionEvent::TextComplete(text) => serde_json::json!({ "text": text }),
        SessionEvent::ThinkingComplete(text) => serde_json::json!({ "text": text }),
        SessionEvent::ToolCallStarted(call) => {
            tally.tool_calls.fetch_add(1, Relaxed);
            // A file-writing tool's arguments carry the path it acts on -
            // the file-mutation evidence for a workspace that is not a git
            // repository.
            if let Some(path) = call.args.get("path").and_then(|v| v.as_str()) {
                if call.name.contains("write") || call.name.contains("edit") {
                    tally.mutated_paths.lock().unwrap().push(path.to_string());
                }
            }
            serde_json::json!({ "call_id": call.id, "name": call.name, "arguments": call.args })
        }
        SessionEvent::ToolCallFinished {
            call_id,
            tool_name,
            output,
            is_error,
        } => {
            if is_error {
                tally.failed_tool_calls.fetch_add(1, Relaxed);
                let first = output
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect::<String>();
                tally
                    .tool_failures
                    .lock()
                    .unwrap()
                    .push(format!("{tool_name}: {first}"));
            }
            serde_json::json!({ "call_id": call_id, "name": tool_name, "is_error": is_error, "output": output })
        }
        SessionEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            turn,
            ..
        } => {
            tally.compactions.fetch_add(1, Ordering::Relaxed);
            serde_json::json!({ "tokens_before": tokens_before, "tokens_after": tokens_after, "turn": turn })
        }
        SessionEvent::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cost_usd,
            ..
        } => {
            tally
                .input_tokens
                .fetch_add(input as u64, Ordering::Relaxed);
            tally
                .output_tokens
                .fetch_add(output as u64, Ordering::Relaxed);
            tally
                .cache_read_tokens
                .fetch_add(cache_read as u64, Ordering::Relaxed);
            tally
                .cache_write_tokens
                .fetch_add(cache_write as u64, Ordering::Relaxed);
            if let Some(cost) = cost_usd {
                tally.saw_cost.store(1, Ordering::Relaxed);
                let e5 = (cost * 100_000.0) as i64;
                if e5 > 0 {
                    tally.cost_e5.fetch_add(e5 as u64, Ordering::Relaxed);
                }
            }
            serde_json::json!({ "input": input, "output": output, "cache_read": cache_read, "cache_write": cache_write })
        }
        SessionEvent::TurnComplete => serde_json::json!({}),
        SessionEvent::Aborted { partial_text } => {
            serde_json::json!({ "partial_text": partial_text })
        }
        SessionEvent::Error(msg) => serde_json::json!({ "message": msg }),
        SessionEvent::TodoUpdate(items) => serde_json::json!({
            "items": items.iter().map(|i| serde_json::json!({ "content": i.content, "status": format!("{:?}", i.status) })).collect::<Vec<_>>(),
        }),
        SessionEvent::Question { id, questions } => {
            tally.asked_questions.fetch_add(1, Ordering::Relaxed);
            serde_json::json!({ "id": id, "questions": questions })
        }
        SessionEvent::ModelChanged(spec) => serde_json::json!({ "spec": spec }),
        _ => serde_json::json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The provider's `input` is fresh-only - tokens served from its prompt
    /// cache are reported separately, and dropping them makes the outcome's
    /// token sums meaningless (a fully cached 30k-token turn reads as 30).
    /// The trace event and the tally must both carry the cache counts.
    #[test]
    fn cache_hits_are_tallied_and_traced() {
        let tally = Tally::default();
        let payload = payload_of(
            SessionEvent::TokenUsage {
                input: 100,
                output: 50,
                cache_read: 4000,
                cache_write: 200,
                cache_read_total: 4000,
                cache_write_total: 200,
                max_tokens: 0,
                max_output_tokens: 0,
                cost_usd: Some(0.01),
            },
            &tally,
        );
        assert_eq!(payload["cache_read"], 4000, "trace keeps the cache counts");
        assert_eq!(payload["cache_write"], 200);
        let usage = tally.usage(0);
        assert_eq!(usage.cache_read_tokens, 4000);
        assert_eq!(usage.cache_write_tokens, 200);
        assert_eq!(usage.cost_usd, Some(0.01));
    }
}
