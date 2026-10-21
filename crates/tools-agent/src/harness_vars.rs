// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The definitions of a harness's variable tools: what an agent step that
//! names variables is offered to list, read and set them.
//!
//! The harness machine answers every call to these tools itself, against what
//! the step was granted, so their `execute` is never reached in a harness run;
//! outside one there is no variable to answer for, and a call says so.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_harness_spec::vars::VAR_TOOLS;
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_vocab::AgentMode;

/// One of the three variable tools.
pub struct HarnessVarTool {
    index: usize,
}

impl HarnessVarTool {
    /// The three tools: list, get, set.
    #[must_use]
    pub fn all() -> Vec<Self> {
        (0..VAR_TOOLS.len()).map(|index| Self { index }).collect()
    }
}

#[async_trait]
impl Tool for HarnessVarTool {
    fn name(&self) -> &str {
        VAR_TOOLS[self.index]
    }

    fn summary(&self) -> &str {
        match self.index {
            0 => "List the run variables this task may read or set.",
            1 => "Read a run variable by name.",
            _ => "Set a run variable this task may write.",
        }
    }

    fn description(&self) -> &str {
        match self.index {
            0 => {
                "Lists the run variables this task can see: each one's name, type, description, \
                 schema, whether you may read or write it, and whether it is set."
            }
            1 => "Returns the current value of a run variable this task can see.",
            _ => {
                "Sets a run variable this task may write. The value must have the variable's \
                 type (and, for a JSON variable, its schema); later tasks read what you set."
            }
        }
    }

    fn parameters_schema(&self) -> Value {
        match self.index {
            0 => json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            1 => json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "The variable's name" } },
                "required": ["name"],
                "additionalProperties": false
            }),
            _ => json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The variable's name" },
                    "value": { "description": "The value: a string, number, boolean or JSON value, as the variable's type says" }
                },
                "required": ["name", "value"],
                "additionalProperties": false
            }),
        }
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    /// Listed in no mode: only a harness step that names variables offers
    /// them.
    fn modes(&self) -> &[AgentMode] {
        &[]
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::err(
            &call.id,
            "run variables exist only in a harness run, whose machine answers this tool",
        )
    }
}
