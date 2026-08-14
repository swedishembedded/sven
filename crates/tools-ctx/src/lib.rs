// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Context/reference tools: the memory-mapped RLM context store
//! (`context_open`/`context_read`/`context_grep`), project knowledge
//! (`list_knowledge`/`search_knowledge`), and the compound `memory` tool
//! that composes both KV memory and the knowledge tools.
//!
//! Split out of `sven-tools`'s `builtin/context/` + `builtin/knowledge/` (5.4
//! of the refactor plan's god-crate splits). The plan's literal grouping was
//! "context+knowledge -> ctx", but the real dependency graph is more precise
//! than that directory-name pairing suggests:
//!
//! - `search_knowledge` physically lived in `builtin/search/` (alongside the
//!   unrelated `grep`/`search_codebase` codebase-search tools), not
//!   `builtin/knowledge/` -- but it and `list_knowledge` are the only two
//!   consumers of `sven_workspace::SharedKnowledge`, so it moved here instead
//!   of following its directory into `sven-tools-web`.
//! - `builtin/system/memory.rs` (`MemoryTool`) turned out to have a *real*,
//!   hard dependency the plan's one-line split didn't name: it directly
//!   constructs and delegates to `ListKnowledgeTool` and `SearchKnowledgeTool`
//!   (`use crate::builtin::{knowledge::list_knowledge::ListKnowledgeTool,
//!   search::search_knowledge::SearchKnowledgeTool}`). That forces `memory.rs`
//!   into this crate too, not the "agent" crate the prior agent's notes
//!   flagged it as a candidate for.
//! - `builtin/context/` itself has zero cross-refs to knowledge/memory (no
//!   shared `sven_workspace` dependency) -- the two subtrees are bundled here
//!   on the plan's original conceptual grouping ("reference material loaded
//!   into the agent's context"), not on a forced code-level coupling.
//!
//! `GrepMatch`, shared with `sven-tools-fs`'s `buffer/store.rs`, moved to
//! `sven-tool-api` (kernel tier) ahead of this split.
pub mod context;
pub mod knowledge;
pub mod memory;

pub use context::{ContextGrepTool, ContextOpenTool, ContextReadTool, ContextStore, SubQueryRunner};
pub use knowledge::{ListKnowledgeTool, SearchKnowledgeTool};
pub use memory::MemoryTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Moved from sven-tools's builtin/mod.rs::output_category_tests along with
// these tools themselves -- see that module's comment for why this contract
// is pinned per-tool at compile time.
#[cfg(test)]
mod output_category_tests {
    use sven_tool_api::tool::{OutputCategory, Tool};

    #[test]
    fn list_knowledge_is_matchlist() {
        let t = super::ListKnowledgeTool {
            knowledge: sven_workspace::SharedKnowledge::empty(),
        };
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn search_knowledge_is_matchlist() {
        let t = super::SearchKnowledgeTool {
            knowledge: sven_workspace::SharedKnowledge::empty(),
        };
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }
}
