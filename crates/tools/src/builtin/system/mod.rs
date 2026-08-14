// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! System and utility tools.
//!
//! `memory` moved to `sven-tools-ctx` (5.4 of the refactor plan's god-crate
//! splits): it directly composes `ListKnowledgeTool`/`SearchKnowledgeTool`,
//! a real dependency the plan's original "agent" grouping for this file
//! didn't account for. `system`/`todo`/`ask_question`/`skill` moved to
//! `sven-tools-agent` (5.6) -- see that crate's doc comment for the judgment
//! calls involved. Only `read_lints` remains, pending the `sven-tools-web`
//! split (5.5), where it will bundle with `grep`/`search_codebase`.

pub mod read_lints;

pub use read_lints::ReadLintsTool;
