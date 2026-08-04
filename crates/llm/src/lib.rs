//! `sven-llm` — LLM conversation primitives for the Sven HSM runtime.
//!
//! # Purpose
//!
//! Provides the [`ConversationStore`] append-only thread store, [`TurnRequest`]
//! for kernel-native single-turn LLM effects, and associated utilities.
//!
//! All LLM interactions are mediated by the HSM kernel via `Effect::CallLlm`
//! with `kind = "turn"`, handled by `TurnExecutor` in `sven-executors`.

pub mod conversation;
pub mod error;

// Re-export the most important types at the crate root.
pub use conversation::{ConversationStore, ThreadId, TurnRequest, TURN_KIND};
pub use error::LlmError;

/// Strip leading/trailing markdown code fences from a string.
///
/// When a model wraps its JSON output in ` ```json … ``` ` fences this helper
/// removes them so the caller can parse the raw JSON.
pub fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    // Handle ```json or ``` prefix
    let s = if let Some(rest) = s.strip_prefix("```") {
        // Skip optional language tag on first line
        if let Some(nl) = rest.find('\n') { &rest[nl + 1..] } else { rest }
    } else {
        s
    };
    // Strip trailing ```
    let s = if let Some(idx) = s.rfind("```") { &s[..idx] } else { s };
    s.trim()
}
