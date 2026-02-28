// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
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
        self.parts
            .iter()
            .any(|p| matches!(p, ToolOutputPart::Image(_)))
    }
}

/// Describes the shape of a tool's text output for context-aware truncation.
///
/// When a tool result exceeds the configured token cap, `sven-core` uses
/// this category to pick the right extraction strategy.  Each tool declares
/// its own category; `sven-core` never hard-codes tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputCategory {
    /// Terminal / process output: keep the first 60 + last 40 lines so both
    /// the command preamble and the final result are visible.
    /// Suitable for: shell, run_terminal_command, gdb commands.
    HeadTail,
    /// Ordered match list: keep the leading matches so the model sees the
    /// highest-relevance results first.
    /// Suitable for: grep, search_codebase, read_lints.
    MatchList,
    /// File content: keep a head and tail window with a separator so the
    /// model sees both the top of the file (imports, declarations) and the
    /// end (recent changes).
    /// Suitable for: read_file, fs read operations.
    FileContent,
    /// Generic text: hard-truncate at the character boundary.
    /// Used for all tools that do not fit the categories above.
    #[default]
    Generic,
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
    /// Describes the shape of this tool's output for context-aware truncation.
    ///
    /// Override this when your tool produces output whose leading or trailing
    /// portion is more useful than a hard cut.  The default is
    /// [`OutputCategory::Generic`] (hard truncation).
    fn output_category(&self) -> OutputCategory {
        OutputCategory::Generic
    }
    /// Execute the tool.  Errors should be wrapped in [`ToolOutput::err`].
    async fn execute(&self, call: &ToolCall) -> ToolOutput;
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::{json, Value};

    use super::*;
    use crate::policy::ApprovalPolicy;

    // -- OutputCategory --

    #[test]
    fn output_category_default_is_generic() {
        assert_eq!(OutputCategory::default(), OutputCategory::Generic);
    }

    #[test]
    fn output_category_variants_are_distinct() {
        assert_ne!(OutputCategory::HeadTail, OutputCategory::MatchList);
        assert_ne!(OutputCategory::HeadTail, OutputCategory::FileContent);
        assert_ne!(OutputCategory::HeadTail, OutputCategory::Generic);
        assert_ne!(OutputCategory::MatchList, OutputCategory::FileContent);
        assert_ne!(OutputCategory::MatchList, OutputCategory::Generic);
        assert_ne!(OutputCategory::FileContent, OutputCategory::Generic);
    }

    #[test]
    fn output_category_copy_semantics() {
        let a = OutputCategory::HeadTail;
        let b = a; // Copy — no move
        assert_eq!(a, b);
    }

    // -- Tool trait default output_category --

    struct MinimalTool;

    #[async_trait]
    impl Tool for MinimalTool {
        fn name(&self) -> &str {
            "minimal"
        }
        fn description(&self) -> &str {
            "a minimal tool"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, "ok")
        }
    }

    #[test]
    fn tool_default_output_category_is_generic() {
        assert_eq!(MinimalTool.output_category(), OutputCategory::Generic);
    }

    struct HeadTailTool;

    #[async_trait]
    impl Tool for HeadTailTool {
        fn name(&self) -> &str {
            "ht"
        }
        fn description(&self) -> &str {
            "produces terminal output"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        fn output_category(&self) -> OutputCategory {
            OutputCategory::HeadTail
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, "ok")
        }
    }

    #[test]
    fn tool_can_override_output_category() {
        assert_eq!(HeadTailTool.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn overridden_category_differs_from_default() {
        assert_ne!(
            HeadTailTool.output_category(),
            MinimalTool.output_category()
        );
    }
}
