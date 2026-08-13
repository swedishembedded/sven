// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::Value;

use sven_config::AgentMode;
use sven_hsm::ToolCapability;
pub use sven_vocab::{OutputCategory, ToolCall, ToolOutput, ToolOutputPart};

use crate::policy::ApprovalPolicy;

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
    /// Whether this tool comes from an external MCP server.
    ///
    /// MCP tools are placed after core tools in the prompt and get their own
    /// cache breakpoint so that toggling MCP servers only invalidates the MCP
    /// section of the prompt cache (not the stable core tools section).
    ///
    /// Built-in tools always return `false`.  [`McpTool`] returns `true`.
    fn is_mcp(&self) -> bool {
        false
    }
    /// The kernel-level capability bucket this tool exercises.
    ///
    /// The kernel uses this for permission gating and audit.  Every built-in
    /// tool overrides this to return the tightest fitting bucket.  The default
    /// (`ReadFile`) is the most conservative non-dangerous capability; MCP
    /// tools override to `NetworkAccess`.
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    /// Execute the tool.  Errors should be wrapped in [`ToolOutput::err`].
    async fn execute(&self, call: &ToolCall) -> ToolOutput;
}
/// Trait for providing display metadata for tools in the TUI.
///
/// All methods have sensible defaults.  Implement only what you need.
///
/// **Note:** methods must return pure data (strings, booleans).  No ratatui
/// types here - styling lives in `sven-tui`.
pub trait ToolDisplay: Send + Sync {
    /// Short display name shown in collapsed view (e.g., "Shell", "Read").
    fn display_name(&self) -> &str;

    /// Single-character (or short) symbol/icon for this tool category.
    ///
    /// Shown before the tool name in collapsed view.  Default: `"▶"`.
    fn icon(&self) -> &str {
        "▶"
    }

    /// Generate a one-line summary for collapsed view.
    ///
    /// `args` is the parsed JSON arguments passed to the tool call.
    /// Return an empty string to show only the display name.
    fn collapsed_summary(&self, _args: &serde_json::Value) -> String {
        String::new()
    }

    /// Whether this tool supports diff rendering in expanded view.
    fn supports_diff(&self) -> bool {
        false
    }

    /// Category hint used by the TUI to apply appropriate styling.
    ///
    /// - `"file"` - file operations (read/write/edit/delete)
    /// - `"shell"` - shell/terminal commands
    /// - `"search"` - search and grep operations
    /// - `"web"` - web fetch and search
    /// - `"system"` - todos, lints, mode changes
    /// - `"agent"` - sub-agent / delegation tools
    /// - `""` - generic / no category
    fn category(&self) -> &str {
        ""
    }
}

/// Registry for looking up display metadata for tools.
pub struct ToolDisplayRegistry {
    displays: std::collections::HashMap<String, std::sync::Arc<dyn ToolDisplay>>,
}

impl ToolDisplayRegistry {
    pub fn new() -> Self {
        Self {
            displays: std::collections::HashMap::new(),
        }
    }

    pub fn register<T: ToolDisplay + 'static>(&mut self, name: impl Into<String>, display: T) {
        self.displays
            .insert(name.into(), std::sync::Arc::new(display));
    }

    /// Register a display by shared reference. Used when the same instance
    /// is registered as a tool and as a display (e.g. via `ToolRegistry::register_with_display`).
    pub fn register_arc(
        &mut self,
        name: impl Into<String>,
        display: std::sync::Arc<dyn ToolDisplay>,
    ) {
        self.displays.insert(name.into(), display);
    }

    pub fn get(&self, name: &str) -> Option<&dyn ToolDisplay> {
        self.displays.get(name).map(|b| b.as_ref())
    }
}

impl Default for ToolDisplayRegistry {
    fn default() -> Self {
        Self::new()
    }
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
        let b = a; // Copy - no move
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
