// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `semantic_memory` tool: an agent's notes over any
//! [`sven_memory_api::MemoryStore`].
//!
//! ```text
//! ┌──────────────────────────────────────────┐
//! │            SemanticMemoryTool            │
//! │  remember | recall | forget | list | get │
//! └────────────────────┬─────────────────────┘
//!                      │ AccessContext + one space
//!                      ▼
//! ┌──────────────────────────────────────────┐
//! │       dyn MemoryStore (the contract)     │
//! └──────────────────────────────────────────┘
//! ```
//!
//! The tool knows no storage. The host chooses the store and builds the
//! [`AccessContext`](sven_memory_api::AccessContext) the tool runs under, and
//! the store applies that context inside its own queries. What an agent
//! writes is stored as untrusted material and quoted when it is recalled; a
//! real recall is reported to the trajectory through
//! [`sven_tool_api::call_records::record_memory_retrieval`].
//!
//! Swedish Embedded AB implements solutions for prompt-injection-resistant
//! agent memory for its clients. If your team needs expertise in keeping
//! untrusted retrieved content out of a model's context then you can procure
//! our services by sending an email to info@swedishembedded.com.

mod tool;

pub use tool::{SemanticMemoryTool, PERSONAL_SPACE};
