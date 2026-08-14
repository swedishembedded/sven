// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Knowledge base tools.
//!
//! `search_knowledge` lived in `sven-tools`'s `builtin/search/` (grouped by
//! directory with the unrelated `grep`/`search_codebase` codebase-search
//! tools) but has always been conceptually a *knowledge* tool: it and
//! `list_knowledge` are the only two consumers of `sven_workspace::SharedKnowledge`,
//! and `MemoryTool` (this crate's `memory` module) composes both of them
//! directly. Placed here, next to `list_knowledge`, rather than following
//! the `search/` directory into `sven-tools-web` -- see this crate's
//! top-level doc comment for the full reasoning.

pub mod list_knowledge;
pub mod search_knowledge;

pub use list_knowledge::ListKnowledgeTool;
pub use search_knowledge::SearchKnowledgeTool;
