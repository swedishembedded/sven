// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The concrete tool registry and the config-driven policy engine built on
//! top of `sven-tool-api`'s trait/type vocabulary.
pub mod policy;
pub mod registry;

pub use policy::ToolPolicy;
pub use registry::{SharedToolDisplays, SharedTools, ToolDisplayInfo, ToolRegistry, ToolSchema};
// Re-export the tool-api vocabulary a registry consumer typically needs
// alongside the registry itself, so `sven_tool_registry::{Tool, ToolCall,
// ApprovalPolicy, ...}` works without an extra `sven-tool-api` dependency.
pub use sven_tool_api::{
    ApprovalPolicy, OutputCategory, PermissionRequester, Tool, ToolCall, ToolDisplay,
    ToolDisplayRegistry, ToolOutput, ToolOutputPart,
};
