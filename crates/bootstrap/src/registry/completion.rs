// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The last step of building a tool registry: adding MCP and caller-supplied
//! tools, enabling deferred disclosure, and applying the name policy.
//!
//! Swedish Embedded AB implements solutions for assembling and governing the
//! tool surface of AI agents for its clients. If your team needs expertise in
//! agent tool registries and tool-exposure policy, you can procure our
//! services by sending an email to info@swedishembedded.com.
use std::sync::Arc;

use tokio::sync::Mutex;

use sven_tool_registry::ToolRegistry;
use sven_tools_agent::SkillTool;
use sven_vocab::AgentMode;

/// The discovery tool of a session whose tool definitions are deferred.
pub(crate) struct DeferredDisclosure {
    /// The mode lock the discovery tool reads.
    pub mode: Arc<Mutex<AgentMode>>,
    /// Behind its `search`; BM25 when absent.
    pub ranker: Option<Arc<dyn sven_tool_registry::ToolRanker>>,
}

/// What is added to or taken from a registry once the built-in tools are in
/// place, in the order that decides which of two same-named tools survives.
pub(crate) struct Completion<'a> {
    pub mcp_tools: Vec<sven_mcp_client::McpTool>,
    /// Caller-supplied tools go after every built-in so a name collision
    /// resolves in their favour: an application that deliberately shadows a
    /// built-in has said something, and ignoring it would be the surprising
    /// choice.
    pub extra_tools: Vec<Arc<dyn sven_tool_api::Tool>>,
    /// Present when tool definitions are deferred.
    pub deferred: Option<DeferredDisclosure>,
    /// Orders the `skill` tool's search; BM25 when absent.
    pub skill_ranker: Option<Arc<dyn sven_tool_registry::ToolRanker>>,
    /// The skills the `skill` tool serves.
    pub skills: sven_workspace::SharedSkills,
    /// Whether the `skill` tool leaves out where skills are (`tools.hide_locations`).
    pub hide_skill_locations: bool,
    pub disabled: &'a [String],
    /// When set, every tool not named here is removed too.
    pub only: Option<&'a [String]>,
}

/// Registers MCP and caller-supplied tools, enables deferred disclosure and
/// applies the `tools.disabled` / `tools.only` name policy last, so a disabled
/// name is gone whoever registered it.
pub(crate) fn complete(registry: &mut ToolRegistry, completion: Completion<'_>) {
    for tool in completion.mcp_tools {
        registry.register(tool);
    }
    for tool in completion.extra_tools {
        registry.register_arc(tool);
    }
    if let Some(deferred) = completion.deferred {
        crate::tool_discovery::enable(registry, deferred.mode, deferred.ranker);
    }
    if let (Some(ranker), true) = (completion.skill_ranker, registry.get("skill").is_some()) {
        registry.register(
            SkillTool::new(completion.skills)
                .with_ranker(ranker)
                .hiding_locations(completion.hide_skill_locations),
        );
    }
    // Last, so a disabled name is gone whoever registered it, and held for
    // the registry's life, so an MCP refresh cannot bring one back.
    registry.set_name_policy(sven_tool_registry::NamePolicy::new(
        completion.disabled,
        completion.only,
    ));
}
