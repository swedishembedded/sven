// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Append-only per-thread conversation storage.
//!
//! A machine keeps one conversation **thread** per state (e.g. `intake`,
//! `discovery`, `task:t1`). Each thread is an append-only `Vec<Message>`:
//! turns are only ever pushed, never rewritten. This is the cache-safety
//! invariant - the provider's prompt cache stays valid because the prefix
//! never changes. Cross-state / cross-submachine context is carried by
//! *appending a new user turn* to the destination thread, never by editing
//! history.

use std::collections::HashMap;
use std::sync::Mutex;

use sven_hsm::{ProposedToolCall, ToolCallId};
use sven_tool_registry::ToolRegistry;

use sven_model::{Message, MessageContent};
use sven_vocab::CompactionRecord;

/// Stable string identifier for a conversation thread (e.g. `"intake"`).
pub type ThreadId = String;

/// Owns one append-only `Vec<Message>` per thread for the lifetime of a runtime.
///
/// Threads are created lazily on first access.  The store never rewrites or
/// removes earlier turns — only [`append`](ThreadStore::append) is
/// exposed for mutation, preserving the cache-safety invariant.
#[derive(Debug, Default)]
pub struct ThreadStore {
    threads: HashMap<ThreadId, Vec<Message>>,
    /// The model's raw argument text, by call id, for tool calls whose
    /// arguments were not valid JSON. The thread holds such a call with `{}`
    /// arguments - history is replayed to providers, and a malformed string
    /// there would be rejected by some - so this is where the original text
    /// is kept for whoever records the run.
    unparsed_arguments: HashMap<String, String>,
    /// Every compaction applied to any thread, oldest first.
    compactions: Vec<CompactionRecord>,
}

impl ThreadStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if the named thread has no turns yet (or does not exist).
    #[must_use]
    pub fn is_empty(&self, id: &str) -> bool {
        self.threads.get(id).map(Vec::is_empty).unwrap_or(true)
    }

    /// `true` if the thread exists (even if it has been created but is empty).
    #[must_use]
    pub fn exists(&self, id: &str) -> bool {
        self.threads.contains_key(id)
    }

    /// Mutable access to a thread's message buffer, creating it if absent.
    ///
    /// Callers must only ever *push* onto the returned buffer; mutating earlier
    /// turns breaks the prompt-cache invariant.
    pub fn thread(&mut self, id: &str) -> &mut Vec<Message> {
        self.threads.entry(id.to_string()).or_default()
    }

    /// Append a single turn to a thread (creating the thread if needed).
    pub fn append(&mut self, id: &str, message: Message) {
        self.threads
            .entry(id.to_string())
            .or_default()
            .push(message);
    }

    /// Replace a thread's entire contents with `messages`.
    ///
    /// Unlike [`append`](Self::append), this is **not** append-only: it is the
    /// history-seeding escape hatch used by the interactive frontends when the
    /// user edits and resubmits an earlier turn (edit-resubmit) or resumes a
    /// saved session. The frontend reconstructs the authoritative history and
    /// installs it here so the next turn streams against exactly those turns,
    /// not the store's own accumulated version. `TurnExecutor` also uses it to
    /// install a compacted history. Because the prefix changes, the provider
    /// prompt cache for this thread is intentionally invalidated.
    pub fn replace_thread(&mut self, id: &str, messages: Vec<Message>) {
        self.threads.insert(id.to_string(), messages);
    }

    /// A read-only clone of a thread's current turns (empty if absent).
    #[must_use]
    pub fn snapshot(&self, id: &str) -> Vec<Message> {
        self.threads.get(id).cloned().unwrap_or_default()
    }

    /// Answers every tool call in `id` that has no result yet, so the thread
    /// is a history a provider accepts. `reasons` maps the model's call id to
    /// why the call was not run; a call without one is answered generically.
    /// Returns how many calls were answered.
    pub fn answer_open_calls(&mut self, id: &str, reasons: &HashMap<String, String>) -> usize {
        let open: Vec<String> = self
            .open_calls(id)
            .into_iter()
            .map(|(call, _)| call.to_string())
            .collect();
        let thread = self.thread(id);
        for call in &open {
            let reason = reasons.get(call).map_or("it was refused", String::as_str);
            thread.push(Message::tool_result(
                call,
                format!("The call was not run: {reason}."),
            ));
        }
        open.len()
    }

    /// The calls in thread `id` that no result answers, as the model made
    /// them: its id for the call and the function it named.
    fn open_calls(&self, id: &str) -> Vec<(&str, &sven_model::FunctionCall)> {
        let Some(thread) = self.threads.get(id) else {
            return Vec::new();
        };
        let answered: std::collections::HashSet<&str> = thread
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        thread
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                } if !answered.contains(tool_call_id.as_str()) => {
                    Some((tool_call_id.as_str(), function))
                }
                _ => None,
            })
            .collect()
    }

    /// The registry entries (`kernel id -> (thread, model id)`) for the calls
    /// of thread `id` that no result answers, recovered after the session
    /// that proposed them is gone.
    ///
    /// The kernel names a call by an id it minted, and only the registry
    /// knows the model's id for it; a machine's record of `proposed` calls
    /// outlives the session, so each is paired with the open call of the same
    /// function and arguments. Two identical calls pair in either order,
    /// which is indistinguishable to everyone.
    #[must_use]
    pub fn recover_open_calls<'a>(
        &self,
        id: &str,
        proposed: impl IntoIterator<Item = &'a ProposedToolCall>,
    ) -> HashMap<ToolCallId, (ThreadId, String)> {
        let mut open = self.open_calls(id);
        let mut recovered = HashMap::new();
        for call in proposed {
            let found = open.iter().position(|(_, function)| {
                function.name == call.name
                    && serde_json::from_str::<serde_json::Value>(&function.arguments)
                        .is_ok_and(|args| args == call.args)
            });
            if let Some(found) = found {
                let (model_id, _) = open.swap_remove(found);
                recovered.insert(call.call_id, (id.to_string(), model_id.to_string()));
            }
        }
        recovered
    }

    /// Keeps the raw argument text of tool calls the model garbled beyond
    /// repair, by call id. See [`unparsed_arguments`](Self::unparsed_arguments).
    pub fn record_unparsed_arguments(&mut self, raw: HashMap<String, String>) {
        self.unparsed_arguments.extend(raw);
    }

    /// Raw argument text, by call id, of every call recorded with
    /// [`record_unparsed_arguments`](Self::record_unparsed_arguments).
    #[must_use]
    pub fn unparsed_arguments(&self) -> &HashMap<String, String> {
        &self.unparsed_arguments
    }

    /// Notes that a thread was compacted. Call alongside
    /// [`replace_thread`](Self::replace_thread) with the compacted history.
    pub fn record_compaction(&mut self, record: CompactionRecord) {
        self.compactions.push(record);
    }

    /// Every compaction recorded so far, oldest first.
    #[must_use]
    pub fn compactions(&self) -> &[CompactionRecord] {
        &self.compactions
    }

    /// Number of turns currently stored in a thread.
    #[must_use]
    pub fn len(&self, id: &str) -> usize {
        self.threads.get(id).map(Vec::len).unwrap_or(0)
    }
}

/// The model's own call id for each refused call, mapped to its reason:
/// the machine names calls by the kernel's id, the thread by the model's.
pub(crate) fn refusal_reasons(
    registry: &Mutex<HashMap<ToolCallId, (String, String)>>,
    refused: &[sven_vocab::RefusedCall],
) -> HashMap<String, String> {
    let Ok(registry) = registry.lock() else {
        return HashMap::new();
    };
    registry
        .iter()
        .filter_map(|(id, (_, model_id))| {
            let id = id.as_uuid().to_string();
            let refusal = refused.iter().find(|r| r.call_id == id)?;
            Some((model_id.clone(), refusal.reason.clone()))
        })
        .collect()
}

/// Register call_id → (thread, original_id) in the registry (non-async).
///
/// The `original_id` is the raw tool_call_id string returned by the LLM (e.g.
/// `"call_HyZJn1bTtqVbmzzS3W4VM4xb"` for OpenAI).  It is preserved so that
/// `ToolExecutor` can append tool results with the *same* id that appears in the
/// assistant message — the API rejects mismatches.
pub(crate) fn register_calls(
    registry: &Mutex<HashMap<ToolCallId, (String, String)>>,
    thread_id: &str,
    calls: &[sven_tool_api::ToolCall],
    tools: &ToolRegistry,
) -> Vec<ProposedToolCall> {
    let mut proposed = Vec::with_capacity(calls.len());
    if let Ok(mut reg) = registry.lock() {
        for tc in calls {
            let capability = tools.capability_of_call(tc);
            let call_id = ToolCallId::from_str_lossy(&tc.id);
            reg.insert(call_id, (thread_id.to_string(), tc.id.clone()));
            proposed.push(ProposedToolCall {
                call_id,
                name: tc.name.clone(),
                args: tc.args.clone(),
                capability,
            });
        }
    }
    proposed
}

/// Snapshot a thread from the store (non-async; guard is never held across await).
pub(crate) fn snapshot_thread(store: &Mutex<ThreadStore>, thread_id: &str) -> Vec<Message> {
    store
        .lock()
        .map(|s| s.snapshot(thread_id))
        .unwrap_or_default()
}

/// Append a batch of messages to a store thread (non-async).
pub(crate) fn append_messages(store: &Mutex<ThreadStore>, thread_id: &str, messages: Vec<Message>) {
    if let Ok(mut s) = store.lock() {
        for msg in messages {
            s.append(thread_id, msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_is_append_only_and_snapshots() {
        let mut store = ThreadStore::new();
        assert!(store.is_empty("intake"));
        store.append("intake", Message::system("role"));
        store.append("intake", Message::user("hi"));
        assert_eq!(store.len("intake"), 2);
        let snap = store.snapshot("intake");
        assert_eq!(snap.len(), 2);
        // Snapshot is a clone; mutating it does not affect the store.
        assert!(!store.is_empty("intake"));
    }

    /// A call with no result is answered with its reason; one that has a
    /// result is left alone.
    #[test]
    fn open_calls_are_answered_once() {
        let call = |id: &str| Message {
            role: sven_model::Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: sven_model::FunctionCall {
                    name: "deploy".into(),
                    arguments: "{}".into(),
                },
            },
        };
        let mut store = ThreadStore::new();
        store.append("t", call("ran"));
        store.append("t", Message::tool_result("ran", "ok"));
        store.append("t", call("refused"));
        let reasons = HashMap::from([("refused".to_string(), "a human said no".to_string())]);
        assert_eq!(store.answer_open_calls("t", &reasons), 1);
        assert_eq!(store.answer_open_calls("t", &reasons), 0);
        let last = format!("{:?}", store.snapshot("t").last().unwrap());
        assert!(last.contains("a human said no"), "{last}");
    }

    #[test]
    fn thread_creates_lazily() {
        let mut store = ThreadStore::new();
        store.thread("discovery").push(Message::user("explore"));
        assert_eq!(store.len("discovery"), 1);
        assert!(store.exists("discovery"));
    }
}
