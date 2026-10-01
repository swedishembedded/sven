// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Context/reference tools: the memory-mapped RLM context store
//! (`context_open`/`context_read`/`context_grep`), project knowledge
//! (`list_knowledge`/`search_knowledge`), and the compound `memory` tool
//! that composes both KV memory and the knowledge tools.
//!
//! `MemoryTool` constructs and delegates to `ListKnowledgeTool` and
//! `SearchKnowledgeTool`, which is why the memory and knowledge tools share
//! this crate; `list_knowledge` and `search_knowledge` are the only
//! consumers of `sven_workspace::SharedKnowledge`. The `context/` tools have
//! no code-level coupling to knowledge/memory and are grouped here as
//! reference material loaded into the agent's context.
//!
//! `GrepMatch`, shared with `sven-tools-fs`'s `buffer/store.rs`, lives in
//! `sven-tool-api` (kernel tier).
pub mod context;
pub mod knowledge;
pub mod memory;

pub use context::{
    ContextGrepTool, ContextOpenTool, ContextReadTool, ContextStore, SubQueryRunner,
};
pub use knowledge::{ListKnowledgeTool, SearchKnowledgeTool};
pub use memory::{MemoryConfig, MemoryTool};

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Pins each tool's declared `OutputCategory`: the executor's truncation
// strategy depends on it, so a silent change would change what the model
// sees of every oversized result.
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
