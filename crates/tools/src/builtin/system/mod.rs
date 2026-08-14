// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! System and utility tools.
//!
//! `memory` moved to `sven-tools-ctx` (5.4 of the refactor plan's god-crate
//! splits): it directly composes `ListKnowledgeTool`/`SearchKnowledgeTool`,
//! a real dependency the plan's original "agent" grouping for this file
//! didn't account for.

pub mod ask_question;
pub mod read_lints;
pub mod skill;
#[allow(clippy::module_inception)]
pub mod system;
pub mod todo;

pub use ask_question::AskQuestionTool;
pub use read_lints::ReadLintsTool;
pub use skill::SkillTool;
pub use system::{ModelCatalogEntry, SystemTool};
pub use todo::TodoTool;
