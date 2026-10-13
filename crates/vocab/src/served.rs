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
