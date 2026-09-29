// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Knowledge base tools.
//!
//! `search_knowledge` and `list_knowledge` are the only two consumers of
//! `sven_workspace::SharedKnowledge`, and `MemoryTool` (this crate's
//! `memory` module) composes both of them directly - see this crate's
//! top-level doc comment.

pub mod list_knowledge;
pub mod search_knowledge;

pub use list_knowledge::ListKnowledgeTool;
pub use search_knowledge::SearchKnowledgeTool;
