// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Structured details of a context-compaction step, kept in the step's
//! `extra.sven` (see [`ContextCompactionDetails`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Structured details for a context-compaction event, preserved at
/// `step.extra.sven` alongside the Section VII `context_management`
/// convention object (which only carries `type`/`boundary`, not the
/// numeric before/after token counts or sven's strategy/turn fields).
///
/// Note: this reuses the `"sven"` extra-object key name at the *step*
/// level, distinct in scope from [`SvenSessionMeta::EXTRA_KEY`](crate::trace_session::SvenSessionMeta::EXTRA_KEY) at the
/// *trajectory root* level — both follow the same "namespace sven's bits
/// under `extra.sven`" convention, just at different nesting depths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionDetails {
    /// Approximate token count before compaction.
    pub tokens_before: usize,
    /// Approximate token count after compaction.
    pub tokens_after: usize,
    /// Which compaction strategy was used (structured/narrative/emergency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Agentic loop round in which compaction fired (0 = pre-submit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
}

impl ContextCompactionDetails {
    /// Extract compaction details from a step's `extra.sven` object, if
    /// present and well-formed.
    pub fn from_step_extra(extra: &Value) -> Option<Self> {
        extra
            .get("sven")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}
