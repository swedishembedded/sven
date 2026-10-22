// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for local model serving for its
// clients: adapters that change under a running agent, and agents that notice.
// If your team needs expertise in self-hosted LLM infrastructure then you can
// procure our services by sending an email to info@swedishembedded.com.
//! What a model server says about the model it serves.

use serde::{Deserialize, Serialize};

/// The adapter (fine-tune overlay) a server currently has applied to a model.
///
/// `digest` changes whenever the adapter's weights change, so comparing two
/// probes' digests detects a hot swap even when `id` stays the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterRef {
    pub id: String,
    pub digest: String,
}

impl std::fmt::Display for AdapterRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.id, self.digest)
    }
}

/// What a server says about the decode that answered one request: the seed it
/// actually used (which differs from the one asked for when none was asked),
/// the adapter and base checkpoint it ran, and what ran them. Each field is
/// absent when the server does not report it; none is ever guessed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedIdentity {
    /// The sampling seed the decode used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// The adapter that answered, by id and content digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<AdapterRef>,
    /// `sha256:<hex>` of the base checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_digest: Option<String>,
    /// The compute backend that ran the decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// The inference engine build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

impl ServedIdentity {
    /// Whether the server reported nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The one-line notice for a [`SessionEvent::ServedModelChanged`](crate::SessionEvent::ServedModelChanged),
/// shared by every surface that prints it.
#[must_use]
pub fn served_model_changed_text(
    model: &str,
    from: Option<&AdapterRef>,
    to: Option<&AdapterRef>,
    context_window: Option<u32>,
) -> String {
    let mut text = format!(
        "model changed: {model} adapter {} -> {}",
        describe_adapter(from),
        describe_adapter(to)
    );
    if let Some(window) = context_window {
        text.push_str(&format!(" (context window {window})"));
    }
    text
}

/// `adapter` as a short phrase for a status line: `none` when no adapter is
/// applied.
#[must_use]
pub fn describe_adapter(adapter: Option<&AdapterRef>) -> String {
    adapter.map_or_else(|| "none".to_string(), ToString::to_string)
}

/// The one-line notice for a [`SessionEvent::ContextBudget`](crate::SessionEvent::ContextBudget),
/// shared by every surface that prints it.
#[must_use]
pub fn context_budget_text(budget: &ContextBudget) -> String {
    let thinking = budget
        .thinking_cap
        .map_or_else(|| "none".to_string(), |cap| format!("{cap} tokens"));
    format!(
        "context budget: window {} tokens; compaction at ~{} tokens of request; \
         {} tokens kept free for the response; notes of at most {} tokens; \
         thinking cap {thinking}",
        budget.window, budget.compaction_at, budget.output_reserve, budget.notes_tokens
    )
}

/// Where a session compacts and what room a response has, in tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextBudget {
    /// The model's context window.
    pub window: u32,
    /// The request size at which the conversation is compacted.
    pub compaction_at: usize,
    /// Tokens of the window kept free for the response, reasoning included.
    pub output_reserve: usize,
    /// Most tokens the notes of a compaction may take.
    pub notes_tokens: usize,
    /// The opt-in cap on thinking tokens; `None` when there is none.
    pub thinking_cap: Option<u32>,
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn the_budget_reads_as_one_line_naming_every_number() {
        let text = context_budget_text(&ContextBudget {
            window: 98_304,
            compaction_at: 58_764,
            output_reserve: 24_576,
            notes_tokens: 10_000,
            thinking_cap: None,
        });
        assert_eq!(
            text,
            "context budget: window 98304 tokens; compaction at ~58764 tokens of request; \
             24576 tokens kept free for the response; notes of at most 10000 tokens; \
             thinking cap none"
        );
        let capped = context_budget_text(&ContextBudget {
            thinking_cap: Some(4096),
            ..ContextBudget {
                window: 1,
                compaction_at: 1,
                output_reserve: 1,
                notes_tokens: 1,
                thinking_cap: None,
            }
        });
        assert!(capped.ends_with("thinking cap 4096 tokens"));
    }
}
