use async_trait::async_trait;
use serde_json::Value;

use sven_config::AgentMode;

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

/// A single content item in a rich tool output.
///
/// Most tools produce only `Text`.  Vision-capable tools (e.g. `read_image`)
/// may produce a mix of `Text` and `Image` items.
#[derive(Debug, Clone)]
pub enum ToolOutputPart {
    /// Plain UTF-8 text.
    Text(String),
    /// Base64 data URL: `data:<mime>;base64,<b64>`.
    Image(String),
}

/// The result of executing a tool.
///
/// ## Backward compatibility
/// `content` is always the plain-text representation of the output (the
/// concatenation of all `Text` parts).  Existing tools and tests that only
/// access `content` continue to work unchanged.
///
/// ## Image support
/// Tools that return images populate `parts` with a mix of [`ToolOutputPart::Text`]
/// and [`ToolOutputPart::Image`] items.  The agent maps these into the
/// appropriate [`sven_model::ToolResultContent`] variant when building the
/// conversation history.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub call_id: String,
    /// Plain-text content — concatenation of all Text parts.
    /// Always set; always readable.  Backward-compatible field.
    pub content: String,
    /// Structured parts (text and/or images).  For tools that only return
    /// text this contains exactly one `Text` part mirroring `content`.
    pub parts: Vec<ToolOutputPart>,
    /// If true, the tool execution failed non-fatally (returned error message).
    pub is_error: bool,
}

impl ToolOutput {
    /// Successful plain-text result.
    pub fn ok(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let text = content.into();
        let call_id = call_id.into();
        Self {
            call_id,
            content: text.clone(),
            parts: vec![ToolOutputPart::Text(text)],
            is_error: false,
        }
    }

    /// Error result containing a plain-text error message.
    pub fn err(call_id: impl Into<String>, msg: impl Into<String>) -> Self {
        let text = msg.into();
        let call_id = call_id.into();
        Self {
            call_id,
            content: text.clone(),
            parts: vec![ToolOutputPart::Text(text)],
            is_error: true,
        }
    }

    /// Result with arbitrary parts (text and/or images).
    ///
    /// `content` is set to the concatenation of all Text parts.
    pub fn with_parts(call_id: impl Into<String>, parts: Vec<ToolOutputPart>) -> Self {
        let text = parts
            .iter()
            .filter_map(|p| match p {
                ToolOutputPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            call_id: call_id.into(),
            content: text,
            parts,
            is_error: false,
        }
    }

    /// Return `true` if this output contains at least one image part.
    pub fn has_images(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, ToolOutputPart::Image(_)))
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
    /// The agent modes in which this tool is available.
    /// Default: all modes (Research, Plan, Agent).
    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }
    /// Execute the tool.  Errors should be wrapped in [`ToolOutput::err`].
    async fn execute(&self, call: &ToolCall) -> ToolOutput;
}
