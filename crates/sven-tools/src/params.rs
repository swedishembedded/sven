// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Helpers for extracting typed parameters from a [`ToolCall`]'s argument map.
//!
//! Using these helpers eliminates the repeated boilerplate of
//! `call.args.get(key).and_then(|v| v.as_str())` + a custom error message
//! that appears across most tool implementations.

use crate::tool::{ToolCall, ToolOutput};

// ── String parameters ─────────────────────────────────────────────────────────

/// Extract a required `&str` parameter, returning a descriptive `ToolOutput::err`
/// (with the full received args for diagnosis) when it is absent.
pub(crate) fn require_str<'a>(call: &'a ToolCall, key: &str) -> Result<&'a str, ToolOutput> {
    call.args.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        let args_preview = serde_json::to_string(&call.args).unwrap_or_else(|_| "null".to_string());
        ToolOutput::err(
            &call.id,
            format!("missing required parameter '{key}'. Received: {args_preview}"),
        )
    })
}

/// Extract an optional `&str` parameter, returning `None` when absent.
pub(crate) fn opt_str<'a>(call: &'a ToolCall, key: &str) -> Option<&'a str> {
    call.args.get(key).and_then(|v| v.as_str())
}

// ── Numeric / boolean parameters ──────────────────────────────────────────────

/// Extract an optional `u64` parameter.
pub(crate) fn opt_u64(call: &ToolCall, key: &str) -> Option<u64> {
    call.args.get(key).and_then(|v| v.as_u64())
}

/// Extract an optional `bool` parameter.
pub(crate) fn opt_bool(call: &ToolCall, key: &str) -> Option<bool> {
    call.args.get(key).and_then(|v| v.as_bool())
}
