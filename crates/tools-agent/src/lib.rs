// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent self-management tools: the compound `system` tool (mode/model
//! switching, MCP server add/remove), `todo` (session task planning),
//! `ask_question` (mid-turn human-in-the-loop questions), and `skill`
//! (load/list the agent's own available skills).
//!
//! All four are "the agent managing itself": task planning, asking the
//! human a question, mode/model switching, and loading its own operating
//! instructions. `skill` loads content into the conversation like
//! `sven-tools-ctx`'s context/knowledge tools, but what it loads is the
//! agent's *own* capabilities rather than external project reference
//! material, which is why it lives here. The `memory` tool lives in
//! `sven-tools-ctx` because it delegates to the knowledge tools.
pub mod ask_question;
pub mod skill;
pub mod system;
pub mod todo;

pub use ask_question::{AskQuestionTool, Question, QuestionRequest};
pub use skill::SkillTool;
pub use system::{ModelCatalogEntry, SystemTool};
pub use todo::TodoTool;
