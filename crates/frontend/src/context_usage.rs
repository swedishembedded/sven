// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for understanding where an LLM
// agent's context window goes for its clients. If your team needs expertise
// in agent context budgets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! What a model request carries, by category, against the context window.
//!
//! [`ContextUsage::compute`] turns the token ledger's attribution of the last
//! request ([`RequestBreakdown`]), the session's [`ContextBudget`] and the
//! tool registry into a plain category -> tokens table that any frontend can
//! draw. Nothing here draws.
//!
//! # Exact and estimated figures
//!
//! The window, the compaction threshold, the response reserve, the notes limit
//! and the thinking cap are the session's own numbers. The per-category token
//! counts are the ledger's estimates (`bytes / 4`, see `sven_turn::ledger`);
//! the provider's reported prompt size, when there is one, is carried beside
//! them as [`ContextUsage::reported_prompt`] and is the authoritative total.
//! Categories the ledger does not measure (thinking) are 0, never hidden.

use sven_tool_registry::{ToolSchema, CORE_TOOLS};
use sven_vocab::{ContextBudget, ReportedUsage, RequestBreakdown};

/// System-prompt sections that hold the index of tools not offered in full.
const DEFERRED_TOOLS_SECTION: &str = "Deferred Tools";
const SKILLS_SECTION: &str = "Skills";
/// Sections that index subagents and the knowledge base.
const INDEX_SECTIONS: [&str; 2] = ["Subagents", "Knowledge Base"];

/// One slice of the window. The declaration order is the order of the legend
/// and of the cells of the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextCategory {
    SystemPrompt,
    CoreTools,
    LoadedTools,
    McpTools,
    Skills,
    Indexes,
    Memory,
    Messages,
    Thinking,
    FreeSpace,
    CompactionBuffer,
    ResponseReserve,
}

impl ContextCategory {
    /// Every category, in legend order.
    pub const ALL: [Self; 12] = [
        Self::SystemPrompt,
        Self::CoreTools,
        Self::LoadedTools,
        Self::McpTools,
        Self::Skills,
        Self::Indexes,
        Self::Memory,
        Self::Messages,
        Self::Thinking,
        Self::FreeSpace,
        Self::CompactionBuffer,
        Self::ResponseReserve,
    ];

    /// The legend text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemPrompt => "System prompt",
            Self::CoreTools => "Tool definitions (core tools)",
            Self::LoadedTools => "Loaded tools",
            Self::McpTools => "MCP tools (deferred + loaded)",
            Self::Skills => "Skills index",
            Self::Indexes => "Subagent and knowledge indexes",
            Self::Memory => "Memory and project notes",
            Self::Messages => "Messages",
            Self::Thinking => "Thinking",
            Self::FreeSpace => "Free space",
            Self::CompactionBuffer => "Compaction buffer",
            Self::ResponseReserve => "Space kept for the response",
        }
    }

    /// Whether the category is request content (as opposed to headroom).
    #[must_use]
    pub fn is_content(self) -> bool {
        !matches!(
            self,
            Self::FreeSpace | Self::CompactionBuffer | Self::ResponseReserve
        )
    }
}

/// What [`ContextUsage::compute`] reads.
pub struct ContextInputs<'a> {
    /// The model's name, as the session shows it.
    pub model: &'a str,
    /// The model's context window in tokens; 0 when unknown.
    pub window: u32,
    /// The session's compaction budget, once it has announced one.
    pub budget: Option<ContextBudget>,
    /// The ledger's attribution of the last request, once one was sent.
    pub breakdown: Option<&'a RequestBreakdown>,
    /// Every tool the session registers, offered or not.
    pub registered_tools: &'a [ToolSchema],
    /// Skills discovered for the session.
    pub skill_count: usize,
}

/// MCP tools, which are offered by summary until the model loads them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McpToolsUsage {
    /// MCP tools registered.
    pub total: usize,
    /// Of those, tools whose full definition the last request carried.
    pub loaded: usize,
}

impl McpToolsUsage {
    /// Tools offered by summary only.
    #[must_use]
    pub fn deferred(&self) -> usize {
        self.total.saturating_sub(self.loaded)
    }
}

/// The context window broken down by category.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextUsage {
    pub model: String,
    /// The context window in tokens; 0 when unknown.
    pub window: u32,
    /// Estimated tokens by category, in [`ContextCategory::ALL`] order. The
    /// three headroom categories are derived from the budget.
    pub tokens: [u32; 12],
    /// The provider's count for the last prompt, when it reported one.
    pub reported_prompt: Option<u32>,
    /// True when a request has been sent, so the figures describe something.
    pub measured: bool,
    pub budget: Option<ContextBudget>,
    pub mcp: McpToolsUsage,
    pub skill_count: usize,
}

impl ContextUsage {
    /// Attribute the last request to categories and derive the headroom.
    #[must_use]
    pub fn compute(inputs: &ContextInputs<'_>) -> Self {
        let window = inputs
            .budget
            .map_or(inputs.window, |b| b.window.max(inputs.window));
        let mut usage = Self {
            model: inputs.model.to_string(),
            window,
            tokens: [0; 12],
            reported_prompt: None,
            measured: inputs.breakdown.is_some(),
            budget: inputs.budget,
            mcp: McpToolsUsage::default(),
            skill_count: inputs.skill_count,
        };
        let is_mcp = |name: &str| {
            inputs
                .registered_tools
                .iter()
                .any(|t| t.is_mcp && t.name == name)
        };
        usage.mcp.total = inputs.registered_tools.iter().filter(|t| t.is_mcp).count();

        if let Some(b) = inputs.breakdown {
            usage.reported_prompt = b
                .reported
                .as_ref()
                .map(ReportedUsage::logical_prompt)
                .filter(|n| *n > 0);
            usage.attribute(b, &is_mcp, inputs.registered_tools);
        }
        usage.derive_headroom();
        usage
    }

    fn attribute(
        &mut self,
        b: &RequestBreakdown,
        is_mcp: &dyn Fn(&str) -> bool,
        registered: &[ToolSchema],
    ) {
        let section = |name: &str| b.system_runtime.get(name).copied().unwrap_or(0);
        // Tools the registry holds but the request left out are deferred; when
        // none is, every definition went out in full and none counts as loaded.
        let withheld = registered
            .iter()
            .any(|t| !b.tool_schemas.contains_key(&t.name));
        let deferred_mode = withheld || b.system_runtime.contains_key(DEFERRED_TOOLS_SECTION);

        let mut core = 0;
        let mut loaded = 0;
        let mut mcp = section(DEFERRED_TOOLS_SECTION);
        for (name, tokens) in &b.tool_schemas {
            if is_mcp(name) {
                mcp += tokens;
                self.mcp.loaded += 1;
            } else if !deferred_mode || CORE_TOOLS.contains(&name.as_str()) {
                core += tokens;
            } else {
                loaded += tokens;
            }
        }
        let indexed = |sections: &[&str]| sections.iter().map(|s| section(s)).sum::<u32>();
        let runtime_total: u32 = b.system_runtime.values().sum();
        let known =
            section(DEFERRED_TOOLS_SECTION) + section(SKILLS_SECTION) + indexed(&INDEX_SECTIONS);

        self.set(ContextCategory::SystemPrompt, b.system_stable);
        self.set(ContextCategory::CoreTools, core);
        self.set(ContextCategory::LoadedTools, loaded);
        self.set(ContextCategory::McpTools, mcp);
        self.set(ContextCategory::Skills, section(SKILLS_SECTION));
        self.set(ContextCategory::Indexes, indexed(&INDEX_SECTIONS));
        // Project context, instructions, git, CI and the dynamic suffix.
        self.set(ContextCategory::Memory, runtime_total - known);
        self.set(
            ContextCategory::Messages,
            b.user
                + b.assistant
                + b.tool_call_arguments.values().sum::<u32>()
                + b.tool_results.values().sum::<u32>()
                + b.media,
        );
    }

    fn derive_headroom(&mut self) {
        let Some(budget) = self.budget else {
            let free = self.window.saturating_sub(self.used());
            self.set(ContextCategory::FreeSpace, free);
            return;
        };
        let reserve = u32::try_from(budget.output_reserve).unwrap_or(u32::MAX);
        let compaction_at = u32::try_from(budget.compaction_at).unwrap_or(u32::MAX);
        // The room between the compaction threshold and the response reserve.
        let buffer = self
            .window
            .saturating_sub(reserve)
            .saturating_sub(compaction_at);
        let free = compaction_at.saturating_sub(self.used());
        self.set(ContextCategory::FreeSpace, free);
        self.set(ContextCategory::CompactionBuffer, buffer);
        self.set(ContextCategory::ResponseReserve, reserve);
    }

    fn set(&mut self, category: ContextCategory, tokens: u32) {
        self.tokens[category as usize] = tokens;
    }

    /// Estimated tokens of one category.
    #[must_use]
    pub fn of(&self, category: ContextCategory) -> u32 {
        self.tokens[category as usize]
    }

    /// Estimated tokens the request carries: every content category.
    #[must_use]
    pub fn used(&self) -> u32 {
        ContextCategory::ALL
            .iter()
            .filter(|c| c.is_content())
            .map(|c| self.of(*c))
            .fold(0, u32::saturating_add)
    }

    /// Percent of the window the request fills, 0 when the window is unknown.
    #[must_use]
    pub fn used_percent(&self) -> f32 {
        if self.window == 0 {
            return 0.0;
        }
        self.used() as f32 * 100.0 / self.window as f32
    }

    /// Tokens of the MCP tools line: the deferred index plus loaded schemas.
    #[must_use]
    pub fn mcp_tokens(&self) -> u32 {
        self.of(ContextCategory::McpTools)
    }

    /// The category of each of `cells` equal cells of the window, in legend
    /// order, so that a category with `t` tokens covers about
    /// `t / window * cells` cells. Cumulative rounding keeps the total exact;
    /// a category too small for one cell shares a boundary and gets none.
    /// Cells beyond the categories (an unknown window) are free space.
    #[must_use]
    pub fn cells(&self, cells: usize) -> Vec<ContextCategory> {
        let mut out = Vec::with_capacity(cells);
        if self.window == 0 {
            out.resize(cells, ContextCategory::FreeSpace);
            return out;
        }
        let mut cumulative: u64 = 0;
        for category in ContextCategory::ALL {
            cumulative += u64::from(self.of(category));
            let end = ((cumulative * cells as u64 + u64::from(self.window) / 2)
                / u64::from(self.window)) as usize;
            while out.len() < end.min(cells) {
                out.push(category);
            }
        }
        out.resize(cells, ContextCategory::FreeSpace);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, is_mcp: bool) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: String::new(),
            parameters: json!({}),
            is_mcp,
        }
    }

    fn budget() -> ContextBudget {
        ContextBudget {
            window: 100_000,
            compaction_at: 80_000,
            output_reserve: 10_000,
            notes_tokens: 5_000,
            thinking_cap: None,
        }
    }

    fn breakdown() -> RequestBreakdown {
        let mut b = RequestBreakdown {
            system_stable: 3_000,
            user: 1_000,
            assistant: 500,
            media: 100,
            ..RequestBreakdown::default()
        };
        b.system_runtime.insert("Skills".into(), 400);
        b.system_runtime.insert("Subagents".into(), 50);
        b.system_runtime.insert("Knowledge Base".into(), 25);
        b.system_runtime.insert("Deferred Tools".into(), 600);
        b.system_runtime.insert("Project Context".into(), 70);
        b.system_runtime.insert("dynamic_suffix".into(), 30);
        b.tool_schemas.insert("shell".into(), 800);
        b.tool_schemas.insert("grep".into(), 200);
        b.tool_schemas.insert("gdb".into(), 300);
        b.tool_schemas.insert("github_search".into(), 150);
        b.tool_call_arguments.insert("shell".into(), 20);
        b.tool_results.insert("shell".into(), 4_000);
        b
    }

    fn registered() -> Vec<ToolSchema> {
        vec![
            tool("shell", false),
            tool("grep", false),
            tool("gdb", false),
            tool("web", false),
            tool("github_search", true),
            tool("github_issue", true),
            tool("github_pr", true),
        ]
    }

    fn compute(b: Option<&RequestBreakdown>) -> ContextUsage {
        ContextUsage::compute(&ContextInputs {
            model: "m",
            window: 100_000,
            budget: Some(budget()),
            breakdown: b,
            registered_tools: &registered(),
            skill_count: 4,
        })
    }

    #[test]
    fn a_request_is_attributed_to_every_category() {
        let b = breakdown();
        let u = compute(Some(&b));
        assert_eq!(u.of(ContextCategory::SystemPrompt), 3_000);
        assert_eq!(u.of(ContextCategory::CoreTools), 1_000);
        assert_eq!(u.of(ContextCategory::LoadedTools), 300);
        assert_eq!(u.of(ContextCategory::McpTools), 750);
        assert_eq!(u.of(ContextCategory::Skills), 400);
        assert_eq!(u.of(ContextCategory::Indexes), 75);
        assert_eq!(u.of(ContextCategory::Memory), 100);
        assert_eq!(u.of(ContextCategory::Messages), 5_620);
        assert_eq!(u.of(ContextCategory::Thinking), 0);
        assert_eq!(u.used(), b.attributed_total());
        assert_eq!(
            u.mcp,
            McpToolsUsage {
                total: 3,
                loaded: 1
            }
        );
        assert_eq!(u.mcp.deferred(), 2);
    }

    #[test]
    fn headroom_follows_the_budget() {
        let u = compute(Some(&breakdown()));
        // 100000 - 10000 reserve - 80000 compaction = 10000 buffer.
        assert_eq!(u.of(ContextCategory::CompactionBuffer), 10_000);
        assert_eq!(u.of(ContextCategory::ResponseReserve), 10_000);
        assert_eq!(u.of(ContextCategory::FreeSpace), 80_000 - u.used());
        let total: u32 = u.tokens.iter().sum();
        assert_eq!(total, 100_000, "the categories tile the window");
    }

    #[test]
    fn before_any_request_everything_is_empty_but_present() {
        let u = compute(None);
        assert!(!u.measured);
        assert_eq!(u.used(), 0);
        assert_eq!(u.of(ContextCategory::FreeSpace), 80_000);
    }

    #[test]
    fn without_deferral_every_non_mcp_tool_is_core() {
        let mut b = breakdown();
        b.system_runtime.remove("Deferred Tools");
        for t in ["web", "github_issue", "github_pr"] {
            b.tool_schemas.insert(t.into(), 10);
        }
        let u = compute(Some(&b));
        assert_eq!(u.of(ContextCategory::LoadedTools), 0);
        assert_eq!(u.of(ContextCategory::CoreTools), 1_310);
    }

    #[test]
    fn the_provider_count_is_carried_beside_the_estimate() {
        let mut b = breakdown();
        b.reported = Some(ReportedUsage {
            input: 12_345,
            ..ReportedUsage::default()
        });
        assert_eq!(compute(Some(&b)).reported_prompt, Some(12_345));
    }

    #[test]
    fn cells_cover_the_window_in_legend_order() {
        let u = compute(Some(&breakdown()));
        let cells = u.cells(100);
        assert_eq!(cells.len(), 100);
        assert_eq!(
            cells
                .iter()
                .filter(|c| **c == ContextCategory::CompactionBuffer)
                .count(),
            10
        );
        assert_eq!(
            cells
                .iter()
                .filter(|c| **c == ContextCategory::ResponseReserve)
                .count(),
            10
        );
        assert_eq!(cells[0], ContextCategory::SystemPrompt);
        assert_eq!(cells[99], ContextCategory::ResponseReserve);
    }

    #[test]
    fn an_unknown_window_is_all_free_space() {
        let u = ContextUsage::compute(&ContextInputs {
            model: "m",
            window: 0,
            budget: None,
            breakdown: None,
            registered_tools: &[],
            skill_count: 0,
        });
        assert!(u.cells(10).iter().all(|c| *c == ContextCategory::FreeSpace));
    }
}
