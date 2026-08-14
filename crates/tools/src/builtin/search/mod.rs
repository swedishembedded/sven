// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Codebase search tools.
//!
//! `search_knowledge` used to live here too, but moved to `sven-tools-ctx`
//! alongside `list_knowledge` (5.4 of the refactor plan's god-crate splits)
//! -- see that crate's doc comment for why the real `SharedKnowledge`
//! coupling won out over this directory grouping.

pub mod grep;
pub mod search_codebase;

pub use grep::GrepTool;
pub use search_codebase::SearchCodebaseTool;
