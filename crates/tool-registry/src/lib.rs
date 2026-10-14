// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The concrete tool registry and the config-driven policy engine built on
//! top of `sven-tool-api`'s trait/type vocabulary.
pub mod analysis;
pub mod descriptor;
pub mod disclosure;
pub mod expansion;
pub mod index;
pub mod policy;
pub mod registry;
pub mod search;

pub use descriptor::{capability_label, ToolDescriptor};
pub use disclosure::{
    Disclosure, OfferedTools, ToolCatalog, ToolOffer, CORE_TOOLS, DISCOVERY_TOOL,
};
pub use expansion::Expansion;
pub use index::FieldWeights;
pub use policy::ToolPolicy;
pub use registry::{SharedToolDisplays, SharedTools, ToolDisplayInfo, ToolRegistry, ToolSchema};
pub use search::{Bm25Ranker, PreparedRanking, RankedTool, RankingParams, ToolRanker};
// Re-export the tool-api vocabulary a registry consumer typically needs
// alongside the registry itself, so `sven_tool_registry::{Tool, ToolCall,
// ApprovalPolicy, ...}` works without an extra `sven-tool-api` dependency.
pub use sven_tool_api::{
    ApprovalPolicy, OutputCategory, PermissionRequester, Tool, ToolCall, ToolCapability,
    ToolDisplay, ToolDisplayRegistry, ToolOutput, ToolOutputPart,
};
