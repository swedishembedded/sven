//! Append-only per-thread conversation storage and the deliberation request.
//!
//! The SDLC deliberation engine keeps one conversation **thread** per state
//! (e.g. `intake`, `discovery`, `task:t1`).  Each thread is an append-only
//! `Vec<Message>`: turns are only ever pushed, never rewritten.  This is the
//! cache-safety invariant — the provider's prompt cache stays valid because the
//! prefix never changes.  Cross-state / cross-submachine context is carried by
//! *appending a new user turn* to the destination thread, never by editing
//! history.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_model::Message;

/// Stable string identifier for a conversation thread (e.g. `"intake"`).
pub type ThreadId = String;

/// Owns one append-only `Vec<Message>` per thread for the lifetime of a runtime.
///
/// Threads are created lazily on first access.  The store never rewrites or
/// removes earlier turns — only [`append`](ConversationStore::append) is
/// exposed for mutation, preserving the cache-safety invariant.
#[derive(Debug, Default)]
pub struct ConversationStore {
    threads: HashMap<ThreadId, Vec<Message>>,
}

impl ConversationStore {
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
        self.threads.entry(id.to_string()).or_default().push(message);
    }

    /// Replace a thread's entire contents with `messages`.
    ///
    /// Unlike [`append`](Self::append), this is **not** append-only: it is the
    /// history-seeding escape hatch used by the interactive frontends when the
    /// user edits and resubmits an earlier turn (edit-resubmit) or resumes a
    /// saved session. The frontend reconstructs the authoritative history and
    /// installs it here so the next turn streams against exactly those turns,
    /// not the store's own accumulated version. Because the prefix changes, the
    /// provider prompt cache for this thread is intentionally invalidated.
    pub fn replace_thread(&mut self, id: &str, messages: Vec<Message>) {
        self.threads.insert(id.to_string(), messages);
    }

    /// A read-only clone of a thread's current turns (empty if absent).
    #[must_use]
    pub fn snapshot(&self, id: &str) -> Vec<Message> {
        self.threads.get(id).cloned().unwrap_or_default()
    }

    /// Number of turns currently stored in a thread.
    #[must_use]
    pub fn len(&self, id: &str) -> usize {
        self.threads.get(id).map(Vec::len).unwrap_or(0)
    }
}

/// The JSON `kind` tag that selects the single-turn streaming engine.
pub const TURN_KIND: &str = "turn";

/// A request for the HSM to run a **single streaming turn**: stream the model
/// against a conversation thread, collect proposed tool calls, and post
/// [`sven_hsm::Event::LlmTurnComplete`] back to the machine.
///
/// The executor (TurnExecutor) handles all I/O, appends the assistant turn to
/// the store, annotates each proposed tool call with its capability, and
/// registers `call_id → thread` in the shared registry.  The machine itself
/// stays pure and only sees the completion event.
///
/// Serialised into [`sven_hsm::Effect::CallLlm`]'s opaque `request` value with
/// an added `kind: "turn"` discriminator (see [`to_value`]).
///
/// [`to_value`]: TurnRequest::to_value
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TurnRequest {
    /// Stable thread id (e.g. `"chat"`, `"discovery"`).
    pub thread: String,
    /// Optional user-turn instruction to append to the thread before streaming.
    ///
    /// When set, `TurnExecutor` calls
    /// `store.append(thread, Message::user(instruction))` before taking the
    /// snapshot, so the machine can initiate the first turn simply by putting
    /// the user's text here rather than managing the ConversationStore directly.
    #[serde(default)]
    pub instruction: String,
    /// Names of the tools this turn is allowed to call.
    ///
    /// When empty AND `all_tools_mode` is set, the executor resolves the full
    /// tool set for the given mode string.
    #[serde(default)]
    pub tools: Vec<String>,
    /// When non-empty and `tools` is empty, resolves all tools for this mode
    /// name (e.g. `"agent"`, `"code"`) via `ToolRegistry::schemas_for_mode`.
    #[serde(default)]
    pub all_tools_mode: String,
    /// Optional JSON Schema for structured output (name under `schema_name`).
    #[serde(default)]
    pub schema: Value,
    /// Short name identifying the schema (for OpenAI strict mode).
    #[serde(default)]
    pub schema_name: String,
    /// Optional per-state model override (resolved via the model resolver).
    #[serde(default)]
    pub model: Option<String>,
    /// Volatile context suffix forwarded to providers that support uncached
    /// system blocks (e.g. Anthropic).
    #[serde(default)]
    pub dynamic_suffix: Option<String>,
    /// Maximum tool-call rounds before a forced wrap-up turn.
    #[serde(default)]
    pub max_tool_rounds: Option<u32>,
}

impl TurnRequest {
    /// Serialise to a [`serde_json::Value`] with the `kind: "turn"` tag added
    /// so the composite executor can route it.
    ///
    /// # Panics
    ///
    /// Panics only if serialisation fails, which cannot happen for this type.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("TurnRequest serialisation infallible");
        if let Some(obj) = v.as_object_mut() {
            obj.insert("kind".into(), Value::String(TURN_KIND.into()));
        }
        v
    }

    /// Deserialise from the opaque value carried by `Effect::CallLlm`.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is not a valid [`TurnRequest`].
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(v)
    }

    /// `true` if `request` carries the turn discriminator.
    #[must_use]
    pub fn is_turn(request: &Value) -> bool {
        request.get("kind").and_then(Value::as_str) == Some(TURN_KIND)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::Message;

    #[test]
    fn store_is_append_only_and_snapshots() {
        let mut store = ConversationStore::new();
        assert!(store.is_empty("intake"));
        store.append("intake", Message::system("role"));
        store.append("intake", Message::user("hi"));
        assert_eq!(store.len("intake"), 2);
        let snap = store.snapshot("intake");
        assert_eq!(snap.len(), 2);
        // Snapshot is a clone; mutating it does not affect the store.
        assert!(!store.is_empty("intake"));
    }

    #[test]
    fn thread_creates_lazily() {
        let mut store = ConversationStore::new();
        store.thread("discovery").push(Message::user("explore"));
        assert_eq!(store.len("discovery"), 1);
        assert!(store.exists("discovery"));
    }

}
