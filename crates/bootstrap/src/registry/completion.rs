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
    /// Its description, fixed once the registry is complete.
    pub index: crate::tool_discovery::DiscoveryIndex,
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
    /// The run profile: a fixed one narrows the registry to its own tools.
    pub profile: crate::RunProfile,
    pub disabled: &'a [String],
    /// When set, every tool not named here is removed too.
    pub only: Option<&'a [String]>,
}

/// Registers MCP and caller-supplied tools, enables deferred disclosure and
/// applies the `tools.disabled` / `tools.only` name policy last, so a disabled
/// name is gone whoever registered it.
pub(crate) fn complete(registry: &mut ToolRegistry, completion: Completion<'_>) {
    let compact = completion.profile == crate::RunProfile::Compact;
    let mcp_names: Vec<String> = completion
        .mcp_tools
        .iter()
        .map(|t| sven_tool_api::Tool::name(t).to_string())
        .collect();
    if compact {
        crate::compact_profile::restrict(registry);
    }
    // A fixed profile offers no MCP tool and no `tools` tool to load from.
    for tool in completion.mcp_tools.into_iter().filter(|_| !compact) {
        registry.register(tool);
    }
    for tool in completion.extra_tools {
        registry.register_arc(tool);
    }
    if let Some(deferred) = completion.deferred.filter(|_| !compact) {
        crate::tool_discovery::enable(registry, deferred.mode, deferred.ranker, deferred.index);
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
    for warning in surface_warnings(&registry.names(), completion.only, compact, &mcp_names) {
        tracing::warn!("{warning}");
    }
}

/// What a session's configuration asked for and did not get, said once at
/// build: the `tools.only` names no registered tool has, and the MCP tools a
/// compact profile leaves out. Either can leave a session without the tool a
/// task depends on (with `tools.only` naming only an MCP tool, with none at
/// all), which must not pass in silence.
fn surface_warnings(
    offered: &[String],
    only: Option<&[String]>,
    compact: bool,
    mcp_names: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();
    if compact && !mcp_names.is_empty() {
        warnings.push(format!(
            "the compact profile offers no MCP tool; not offered: {}",
            mcp_names.join(", ")
        ));
    }
    if let Some(only) = only {
        let missing: Vec<&str> = only
            .iter()
            .filter(|name| !offered.contains(name))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            warnings.push(format!(
                "tools.only names tools this session does not offer: {}{}",
                missing.join(", "),
                if offered.is_empty() {
                    "; the session offers no tool at all"
                } else {
                    ""
                }
            ));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::surface_warnings;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn an_allow_list_naming_an_mcp_tool_a_compact_profile_dropped_is_reported() {
        let warnings =
            surface_warnings(&[], Some(&names(&["vim-vim"])), true, &names(&["vim-vim"]));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("compact profile offers no MCP tool")
                && warnings[0].contains("vim-vim")
        );
        assert!(warnings[1].contains("vim-vim") && warnings[1].contains("no tool at all"));
    }

    #[test]
    fn a_session_that_has_everything_it_asked_for_says_nothing() {
        let offered = names(&["vim-vim"]);
        assert!(surface_warnings(&offered, Some(&offered), false, &offered).is_empty());
        assert!(surface_warnings(&names(&["read_file"]), None, true, &[]).is_empty());
    }
}
