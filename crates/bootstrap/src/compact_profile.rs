// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running language-model agents on
// models with very small context windows for its clients. If your team needs
// expertise in making a small model a usable agent then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The `compact` run profile (`agent.profile: compact`): the request a
//! small-context model is sent, fixed down to the byte.
//!
//! A session under this profile sends a short system prompt, the definitions
//! of three tools (`read_file`, `write_file`, `shell`) and the conversation.
//! There is no tool index, no skills, memory or project knowledge, and no
//! todo, task or context tools. The tools are the ordinary ones: a compact
//! definition only replaces what the model reads (its description and a
//! parameter schema with the optional parameters left out); the call is
//! checked, gated, run and audited by the tool it wraps.
//!
//! The prompt and the definitions are data a consumer may depend on: a
//! trajectory records [`ID`], and a test pins the exact text. Changing either
//! means raising the version in [`ID`].

use std::sync::Arc;

use crate::RunProfile;
use async_trait::async_trait;
use serde_json::{json, Value};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{OutputCategory, Tool, ToolCall, ToolOutput};
use sven_vocab::AgentMode;

/// The profile's identifier, `<name>/<version>`, recorded in a trajectory's
/// `agent.extra.sven.profile`.
pub const ID: &str = "compact/2";

/// The tools the profile offers, in the order a request lists them.
pub const TOOLS: [&str; 3] = ["read_file", "write_file", "shell"];

/// The whole system prompt of a `compact` session.
pub const SYSTEM_PROMPT: &str = "\
You are a coding agent working in a workspace directory. You act only through tools.

Rules:
- Paths are relative to the workspace. Never use paths outside it.
- Facts come from files and command output, never from memory. Read before you answer.
- Call one tool at a time with valid JSON arguments. Read its result before the next step.
- If a tool returns an error, correct the arguments and try again.
- When the task names a file for your work or answer, write it with write_file. Then reply with a short final message and call no tool. If the task asks for the answer in your reply, give only that answer.";

/// What a compact definition says in place of the tool's own description and
/// schema.
struct Compact {
    description: &'static str,
    schema: fn() -> Value,
}

fn definition(name: &str) -> Option<Compact> {
    match name {
        "read_file" => Some(Compact {
            description: "Read a text file and return its contents.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                })
            },
        }),
        "write_file" => Some(Compact {
            description: "Write text to a file, replacing it. Creates the file if missing.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}, "text": {"type": "string"}},
                    "required": ["path", "text"]
                })
            },
        }),
        "shell" => Some(Compact {
            description: "Run a shell command in the workspace and return its output.",
            schema: || {
                json!({
                    "type": "object",
                    "properties": {"shell_command": {"type": "string"}},
                    "required": ["shell_command"]
                })
            },
        }),
        _ => None,
    }
}

/// Fixes the system prompt of a session under the profile: the profile's own
/// text, with none of sven's identity, guidelines, project, skill or
/// knowledge sections around it. A prompt the caller supplied
/// (`--system-prompt-file`, `agent.system_prompt`) replaces the profile's
/// text, and `--append-system-prompt` still follows it.
pub(crate) fn fix_prompt(profile: RunProfile, runtime: &mut sven_turn::AgentRuntimeContext) {
    if profile != RunProfile::Compact {
        return;
    }
    runtime.no_system = true;
    if runtime.system_prompt_override.is_none() {
        runtime.system_prompt_override = Some(SYSTEM_PROMPT.to_string());
    }
}

/// Leaves `registry` with the profile's tools, as the profile defines them,
/// and nothing else.
pub(crate) fn restrict(registry: &mut sven_tool_registry::ToolRegistry) {
    for name in registry.names() {
        if !TOOLS.contains(&name.as_str()) {
            registry.remove(&name);
        }
    }
    for name in TOOLS {
        if let Some(compact) = registry.get(name).and_then(compacted) {
            registry.register_arc(compact);
        }
    }
}

/// `tool` as the profile presents it, or `None` for a tool the profile does
/// not offer.
#[must_use]
pub fn compacted(tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
    let compact = definition(tool.name())?;
    Some(Arc::new(CompactTool {
        schema: (compact.schema)(),
        description: compact.description,
        inner: tool,
    }))
}

/// A tool whose model-facing definition is the profile's and whose behaviour
/// is entirely the wrapped tool's.
struct CompactTool {
    inner: Arc<dyn Tool>,
    description: &'static str,
    schema: Value,
}

#[async_trait]
impl Tool for CompactTool {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.description
    }
    fn summary(&self) -> &str {
        self.inner.summary()
    }
    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }
    /// The fewest tokens a definition can have: no intent to fill in.
    fn offers_intent(&self) -> bool {
        false
    }
    fn default_policy(&self) -> ApprovalPolicy {
        self.inner.default_policy()
    }
    fn modes(&self) -> &[AgentMode] {
        self.inner.modes()
    }
    fn output_category(&self) -> OutputCategory {
        self.inner.output_category()
    }
    fn is_mcp(&self) -> bool {
        self.inner.is_mcp()
    }
    fn kernel_capability(&self) -> ToolCapability {
        self.inner.kernel_capability()
    }
    fn longest_run(&self) -> Option<std::time::Duration> {
        self.inner.longest_run()
    }
    fn call_capability(&self, args: &Value) -> ToolCapability {
        self.inner.call_capability(args)
    }
    fn activated_tools(&self, args: &Value) -> Vec<String> {
        self.inner.activated_tools(args)
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.inner.execute(call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The system prompt costs under 300 tokens, estimated as the token ledger
    /// does (four bytes to a token).
    #[test]
    fn the_system_prompt_is_under_three_hundred_tokens() {
        assert!(
            SYSTEM_PROMPT.len().div_ceil(4) < 300,
            "{}",
            SYSTEM_PROMPT.len()
        );
    }

    /// The definitions a request carries, pinned. A change here is a change of
    /// what an episode recorded under [`ID`] means: raise the version with it.
    #[test]
    fn the_definitions_are_pinned_to_the_version() {
        let schemas: Vec<_> = TOOLS
            .iter()
            .map(|name| {
                let compact = definition(name).expect("every profile tool has a definition");
                json!({
                    "name": name,
                    "description": compact.description,
                    "parameters": (compact.schema)(),
                })
            })
            .collect();
        assert_eq!(
            Value::Array(schemas),
            json!([
                {"name": "read_file",
                 "description": "Read a text file and return its contents.",
                 "parameters": {"type": "object",
                                "properties": {"path": {"type": "string"}},
                                "required": ["path"]}},
                {"name": "write_file",
                 "description": "Write text to a file, replacing it. Creates the file if missing.",
                 "parameters": {"type": "object",
                                "properties": {"path": {"type": "string"},
                                               "text": {"type": "string"}},
                                "required": ["path", "text"]}},
                {"name": "shell",
                 "description": "Run a shell command in the workspace and return its output.",
                 "parameters": {"type": "object",
                                "properties": {"shell_command": {"type": "string"}},
                                "required": ["shell_command"]}},
            ])
        );
        assert_eq!(ID, "compact/2");
    }
}
