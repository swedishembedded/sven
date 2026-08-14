// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent self-management tools: the compound `system` tool (mode/model
//! switching, MCP server add/remove), `todo` (session task planning),
//! `ask_question` (mid-turn human-in-the-loop questions), and `skill`
//! (load/list the agent's own available skills).
//!
//! Split out of `sven-tools`'s `builtin/system/{system,todo,ask_question,skill}.rs`
//! (5.6 of the refactor plan's god-crate splits). The plan's original terse
//! listing named only `system.rs` for this tier and left
//! `skill.rs`/`memory.rs`/`todo.rs`/`ask_question.rs`/`read_lints.rs` as an
//! open judgment call (they don't map to any of the plan's other 6 names).
//! Verified per-file rather than guessed:
//!
//! - `memory.rs` turned out to have a real, hard dependency on the knowledge
//!   tools (it directly constructs `ListKnowledgeTool`/`SearchKnowledgeTool`)
//!   that forced it into `sven-tools-ctx` instead (see that crate's doc
//!   comment).
//! - `read_lints.rs` has no cross-refs to anything here; it stays behind in
//!   `sven-tools` pending the `sven-tools-web` split, where it will bundle
//!   with `grep`/`search_codebase` (all three are read-only
//!   codebase-diagnostic tools that shell out to an external binary).
//! - `todo.rs` and `ask_question.rs` have zero cross-refs to anything else in
//!   `builtin/`; both are squarely "the agent managing its own turn" (task
//!   planning, asking the human a question) and group naturally with
//!   `system.rs`'s mode/model self-modification.
//! - `skill.rs` (`SkillTool`) was the closest call: it loads reference
//!   content (`SKILL.md` bodies) into the conversation, which reads like
//!   `sven-tools-ctx`'s context/knowledge tools. But unlike those, what it
//!   loads is the agent's *own* operating instructions/capabilities, not
//!   external project reference material -- the same "agent introspects and
//!   configures itself" category as mode/model switching and todo planning,
//!   not "look something up." Kept here on that distinction; zero code-level
//!   coupling forces either placement.
pub mod ask_question;
pub mod skill;
pub mod system;
pub mod todo;

pub use ask_question::{AskQuestionTool, Question, QuestionRequest};
pub use skill::SkillTool;
pub use system::{ModelCatalogEntry, SystemTool};
pub use todo::TodoTool;
