use async_trait::async_trait;
use serde_json::Value;

use crate::policy::ApprovalPolicy;

/// A single tool invocation requested by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Opaque identifier returned by the model (forwarded verbatim)
    pub id: String,
    pub name: String,
    /// Parsed JSON arguments
    pub args: Value,
}

/// The result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub call_id: String,
    /// Human/model readable result text
    pub content: String,
    /// If true, the tool execution failed non-fatally (returned error message)
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self { call_id: call_id.into(), content: content.into(), is_error: false }
    }

    pub fn err(call_id: impl Into<String>, msg: impl Into<String>) -> Self {
        Self { call_id: call_id.into(), content: msg.into(), is_error: true }
    }
}

/// Trait that every built-in and user-defined tool must implement.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema for parameters
    fn parameters_schema(&self) -> Value;
    /// Default approval level for this tool
    fn default_policy(&self) -> ApprovalPolicy;
    /// Execute the tool.  Errors should be wrapped in [`ToolOutput::err`].
    async fn execute(&self, call: &ToolCall) -> ToolOutput;
}
