// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Abstraction for the sub-query capability used by `context_query` and
//! `context_reduce`.
//!
//! Defined here in `sven-tools` so that the read-only context tools can hold
//! a reference to a `SubQueryRunner` without depending on `sven-model`.  The
//! concrete implementation [`ModelSubQueryRunner`] lives in `sven-bootstrap`
//! where `sven-model` is available.

use async_trait::async_trait;

/// Minimal interface for dispatching a single LLM sub-query.
///
/// Each call maps to a stateless completion: a system message plus a user
/// message, with **no tools and no conversation history**.  This matches the
/// `llm_query()` function in the RLM paper's REPL environment.
///
/// Implementors are expected to be cheaply cloneable (e.g. wrap the inner
/// state in `Arc`).
#[async_trait]
pub trait SubQueryRunner: Send + Sync {
    /// Send `prompt` to the LLM and return the text response.
    ///
    /// * `system` — a stable system instruction passed as the system message.
    /// * `prompt` — the user message, typically containing a chunk of content
    ///   followed by the analysis instruction.
    ///
    /// Returns `Ok(text)` on success, `Err(message)` on any failure.
    async fn query(&self, system: &str, prompt: &str) -> Result<String, String>;
}
