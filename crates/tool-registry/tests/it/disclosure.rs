// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a registry that discloses tools on demand offers the model a
//! small core set plus the tools it has loaded, in an order that only ever
//! grows, and keeps the legacy full listing available unchanged.

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_hsm::ToolCapability;
use sven_tool_registry::{
    ApprovalPolicy, Disclosure, Tool, ToolCall, ToolOffer, ToolOutput, ToolRegistry, ToolSchema,
    DISCOVERY_TOOL,
};
use sven_vocab::AgentMode;

/// A tool with a deliberately bulky schema, so the cost of offering it shows.
struct Stub {
    name: &'static str,
    capability: ToolCapability,
    mcp: bool,
    modes: &'static [AgentMode],
}

fn stub(name: &'static str) -> Stub {
    Stub {
        name,
        capability: ToolCapability::ReadFile,
        mcp: false,
        modes: &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent],
    }
}

#[async_trait]
impl Tool for Stub {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "A long description that a model would otherwise read on every request. Details follow."
    }
    fn summary(&self) -> &str {
        "A stub tool for the disclosure spec."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {"x": {"type": "string", "description": "x ".repeat(60)}}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn modes(&self) -> &[AgentMode] {
        self.modes
    }
    fn is_mcp(&self) -> bool {
        self.mcp
    }
    fn kernel_capability(&self) -> ToolCapability {
        self.capability
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "ran")
    }
}

fn registry(disclosure: Disclosure) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    for name in [
        DISCOVERY_TOOL,
        "shell",
        "read_file",
        "write_file",
        "web_fetch",
        "task",
        "skill",
    ] {
        reg.register(stub(name));
    }
    reg.register(Stub {
        mcp: true,
        capability: ToolCapability::NetworkAccess,
        ..stub("files-search")
    });
    reg.set_disclosure(disclosure);
    reg
}

fn offer<'a>(active: &'a [String]) -> ToolOffer<'a> {
    ToolOffer {
        names: &[],
        all_tools_mode: "agent",
        active,
    }
}

fn names(schemas: &[ToolSchema]) -> Vec<&str> {
    schemas.iter().map(|s| s.name.as_str()).collect()
}

fn tokens(schemas: &[ToolSchema]) -> usize {
    schemas
        .iter()
        .map(|s| (s.name.len() + s.description.len() + s.parameters.to_string().len()).div_ceil(4))
        .sum()
}

#[test]
fn a_core_only_request_carries_fewer_schema_tokens_than_the_full_one() {
    let full = registry(Disclosure::Full).offer(&offer(&[])).schemas;
    let deferred = registry(Disclosure::Deferred).offer(&offer(&[])).schemas;

    assert_eq!(
        names(&deferred),
        ["read_file", "shell", "tools", "write_file"],
        "core tools, sorted by name; nothing deferred, MCP included"
    );
    assert!(
        tokens(&deferred) * 2 < tokens(&full),
        "core-only {} vs full {}",
        tokens(&deferred),
        tokens(&full)
    );
}

#[test]
fn full_disclosure_is_the_legacy_listing_exactly() {
    let reg = registry(Disclosure::Full);
    let legacy = reg.schemas_for_mode(AgentMode::Agent);
    let offered = reg.offer(&offer(&["web_fetch".to_string()])).schemas;
    assert_eq!(offered, legacy);
    assert!(reg.offer(&offer(&[])).withheld.is_empty());
}

#[test]
fn loading_adds_the_schema_after_the_core_tools_in_load_order() {
    let reg = registry(Disclosure::Deferred);
    let active: Vec<String> = ["web_fetch", "files-search", "skill"]
        .map(String::from)
        .into();

    let offered = reg.offer(&offer(&active));
    assert_eq!(
        names(&offered.schemas),
        [
            "read_file",
            "shell",
            "tools",
            "write_file",
            "web_fetch",
            "files-search",
            "skill"
        ],
        "core first and stable, then loaded tools exactly as they were loaded"
    );

    // Loading one more tool only appends: the list before it is a prefix of
    // the list after it, which is what lets a provider keep its cached prefix.
    let more: Vec<String> = active.iter().cloned().chain(["task".into()]).collect();
    let later = reg.offer(&offer(&more)).schemas;
    assert_eq!(&later[..offered.schemas.len()], &offered.schemas[..]);
    assert_eq!(later.last().map(|s| s.name.as_str()), Some("task"));
}

#[test]
fn what_is_not_loaded_is_withheld_and_what_is_loaded_is_not() {
    let reg = registry(Disclosure::Deferred);
    let offered = reg.offer(&offer(&["task".to_string()]));
    assert_eq!(offered.withheld, ["files-search", "skill", "web_fetch"]);
}

#[test]
fn a_tool_outside_the_mode_is_never_offered_even_when_loaded() {
    let mut reg = registry(Disclosure::Deferred);
    reg.register(Stub {
        modes: &[AgentMode::Agent],
        ..stub("agent_only")
    });
    let active = ["agent_only".to_string()];
    let research = reg
        .offer(&ToolOffer {
            names: &[],
            all_tools_mode: "research",
            active: &active,
        })
        .schemas;
    assert!(!names(&research).contains(&"agent_only"));
}

#[test]
fn tools_a_state_names_explicitly_are_offered_in_full_whatever_their_class() {
    let reg = registry(Disclosure::Deferred);
    let wanted = ["task".to_string(), "read_file".to_string()];
    let offered = reg.offer(&ToolOffer {
        names: &wanted,
        all_tools_mode: "",
        active: &[],
    });
    assert_eq!(names(&offered.schemas), ["read_file", "task"]);
    assert!(offered.withheld.is_empty());
}

#[test]
fn without_a_discovery_tool_nothing_is_withheld() {
    // Deferring tools nobody can load would strand them.
    let mut reg = registry(Disclosure::Deferred);
    assert!(reg.remove(DISCOVERY_TOOL));
    let offered = reg.offer(&offer(&[]));
    assert_eq!(offered.schemas.len(), 7);
    assert!(offered.withheld.is_empty());
}

#[test]
fn a_call_held_back_is_reported_once() {
    let reg = registry(Disclosure::Deferred);
    let call = sven_hsm::ToolCallId::new();
    reg.hold_back(call);
    assert!(reg.take_held_back(call));
    assert!(!reg.take_held_back(call));
}
