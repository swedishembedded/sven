// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sven_config::AgentMode;

use crate::policy::PermissionRequester;
use crate::tool::ToolDisplayRegistry;
use crate::{ApprovalPolicy, OutputCategory, Tool, ToolCall, ToolOutput};

/// A tool schema – mirrors sven_model::ToolSchema but keeps tools crate
/// independent from the model crate.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// Whether this tool comes from an external MCP server.
    ///
    /// MCP tools are placed after core tools in the prompt and get their own
    /// Anthropic cache breakpoint (BP2) so that toggling servers only
    /// invalidates the MCP section, not the stable core tools section (BP1).
    pub is_mcp: bool,
}

/// Display metadata for a tool, used by the TUI for custom rendering.
#[derive(Debug, Clone)]
pub struct ToolDisplayInfo {
    /// The display name shown in collapsed view (e.g., "Shell", "Read").
    pub display_name: String,
    /// Whether this tool supports diff rendering in expanded view.
    pub supports_diff: bool,
    /// The name of the field that contains the "intent" description.
    pub intent_field: Option<String>,
}

/// Shared, atomically-replaceable snapshot of the agent's tool registry.
///
/// Works exactly like [`sven_runtime::SharedSkills`] and
/// [`sven_runtime::SharedAgents`]: callers hold a cheap `Clone` and call
/// `.get()` to obtain an `Arc<[ToolSchema]>` snapshot without locking.
///
/// The store is populated by [`AgentBuilder`] after the registry is built so
/// that the TUI can list available tools via `/tools` without reaching into
/// the agent's internals.
pub type SharedTools = sven_runtime::Shared<ToolSchema>;

/// Slot for the TUI to receive the tool display registry after the agent is built.
///
/// The builder calls [`SharedToolDisplays::set`] **once** at startup; the TUI
/// holds a cheap clone and calls [`SharedToolDisplays::get`] when rendering.
/// Using `RwLock` (rather than `Mutex`) allows many concurrent readers.
#[derive(Clone, Default)]
pub struct SharedToolDisplays(
    std::sync::Arc<
        std::sync::RwLock<Option<std::sync::Arc<std::sync::RwLock<ToolDisplayRegistry>>>>,
    >,
);

impl SharedToolDisplays {
    /// Create an empty (uninitialized) slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the registry.  Should be called exactly once, by the agent builder.
    pub fn set(&self, registry: std::sync::Arc<std::sync::RwLock<ToolDisplayRegistry>>) {
        if let Ok(mut guard) = self.0.write() {
            *guard = Some(registry);
        }
    }

    /// Return the inner `Arc<RwLock<ToolDisplayRegistry>>`, if set.
    pub fn get(&self) -> Option<std::sync::Arc<std::sync::RwLock<ToolDisplayRegistry>>> {
        self.0.read().ok()?.as_ref().cloned()
    }
}

/// Central registry holding all available tools.
///
/// `ToolRegistry` is automatically `Sync` because `HashMap<String, Arc<dyn Tool>>`
/// is `Sync` when `dyn Tool: Send + Sync`, which is guaranteed by the `Tool`
/// supertrait bounds (`Tool: Send + Sync`).  No manual `unsafe impl` is needed.
///
/// The tools map is behind `RwLock` so MCP tools can be replaced at runtime when
/// servers connect/disconnect or tools are reloaded.
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
    /// Shared so the TUI can hold a clone for chat rendering without owning the registry.
    display_registry: Arc<RwLock<ToolDisplayRegistry>>,
    /// Optional permission requester wired up by the ACP server layer.
    /// When set, tools with `ApprovalPolicy::Ask` are gated behind a
    /// `session/request_permission` round-trip to the IDE before executing.
    permission_requester: Option<Arc<dyn PermissionRequester>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            display_registry: Arc::new(RwLock::new(ToolDisplayRegistry::new())),
            permission_requester: None,
        }
    }

    /// Wire up an IDE-backed permission requester.
    ///
    /// After this call, every `execute` invocation on a tool whose
    /// `default_policy` is [`ApprovalPolicy::Ask`] will block until the IDE
    /// responds to the `session/request_permission` request.
    pub fn set_permission_requester(&mut self, requester: Arc<dyn PermissionRequester>) {
        self.permission_requester = Some(requester);
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        if let Ok(mut guard) = self.tools.write() {
            guard.insert(tool.name().to_string(), Arc::new(tool));
        }
    }

    /// Register a tool that also provides display metadata. The same instance
    /// is used for execution and for TUI display (collapsed summary, display name).
    pub fn register_with_display(&mut self, tool: impl Tool + crate::tool::ToolDisplay + 'static) {
        let arc = Arc::new(tool);
        let name = arc.name().to_string();
        if let Ok(mut guard) = self.tools.write() {
            guard.insert(name.clone(), Arc::clone(&arc) as Arc<dyn Tool>);
        }
        if let Ok(mut disp) = self.display_registry.write() {
            disp.register_arc(name, arc as Arc<dyn crate::tool::ToolDisplay>);
        }
    }

    /// Replace all MCP tools with the given set.  Call when MCP servers connect,
    /// disconnect, or tools are reloaded so the agent uses the updated list.
    pub fn replace_mcp_tools(&self, new_tools: Vec<Arc<dyn Tool>>) {
        if let Ok(mut guard) = self.tools.write() {
            guard.retain(|_, t| !t.is_mcp());
            for tool in new_tools {
                guard.insert(tool.name().to_string(), tool);
            }
        }
    }

    /// Shared handle to the display registry for TUI rendering (collapsed preview, etc.).
    pub fn display_registry(&self) -> Arc<RwLock<ToolDisplayRegistry>> {
        Arc::clone(&self.display_registry)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().ok()?.get(name).cloned()
    }

    /// Produce schemas for ALL registered tools (mode-unfiltered).
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.schemas_filtered(|_| true)
    }

    /// Produce schemas only for tools available in the given mode.
    pub fn schemas_for_mode(&self, mode: AgentMode) -> Vec<ToolSchema> {
        self.schemas_filtered(|t| t.modes().contains(&mode))
    }

    pub async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let tool = match self
            .tools
            .read()
            .ok()
            .and_then(|g| g.get(&call.name).cloned())
        {
            Some(t) => t,
            None => return ToolOutput::err(&call.id, format!("unknown tool: {}", call.name)),
        };
        if let Some(ref requester) = self.permission_requester {
            if matches!(tool.default_policy(), ApprovalPolicy::Ask)
                && !requester.request_permission(call).await
            {
                return ToolOutput::err(
                    &call.id,
                    format!("tool '{}' was denied by the IDE", call.name),
                );
            }
        }
        tool.execute(call).await
    }

    pub fn names(&self) -> Vec<String> {
        self.tools
            .read()
            .ok()
            .map_or_else(Vec::new, |g| g.keys().cloned().collect())
    }

    /// Returns the [`OutputCategory`] for the named tool, or
    /// [`OutputCategory::Generic`] if the tool is not registered.
    pub fn output_category(&self, tool_name: &str) -> OutputCategory {
        self.tools
            .read()
            .ok()
            .and_then(|g| g.get(tool_name))
            .map(|t| t.output_category())
            .unwrap_or_default()
    }

    pub fn names_for_mode(&self, mode: AgentMode) -> Vec<String> {
        let mut names: Vec<String> = self.tools.read().ok().map_or_else(Vec::new, |g| {
            g.values()
                .filter(|t| t.modes().contains(&mode))
                .map(|t| t.name().to_string())
                .collect()
        });
        names.sort();
        names
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Build a sorted schema list, keeping only tools that satisfy `predicate`.
    ///
    /// Core tools (non-MCP) are listed first, sorted by name.
    /// MCP tools follow, also sorted by name.
    /// This ordering ensures stable cache breakpoints: BP1 = end of core tools,
    /// BP2 = end of MCP tools.
    fn schemas_filtered(&self, predicate: impl Fn(&Arc<dyn Tool>) -> bool) -> Vec<ToolSchema> {
        let mut core: Vec<ToolSchema> = Vec::new();
        let mut mcp: Vec<ToolSchema> = Vec::new();

        let guard = match self.tools.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        for t in guard.values() {
            if !predicate(t) {
                continue;
            }
            let schema = ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
                is_mcp: t.is_mcp(),
            };
            if t.is_mcp() {
                mcp.push(schema);
            } else {
                core.push(schema);
            }
        }
        drop(guard);

        core.sort_by(|a, b| a.name.cmp(&b.name));
        mcp.sort_by(|a, b| a.name.cmp(&b.name));
        core.extend(mcp);
        core
    }
}

impl Default for ToolRegistry {
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
    use crate::tool::{Tool, ToolCall, ToolOutput};

    /// Minimal no-op tool for registry tests.
    struct EchoTool {
        name: &'static str,
    }

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "echoes its input"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, format!("echo:{}", call.args))
        }
    }

    /// Tool that explicitly declares a non-default output category.
    struct TerminalTool;

    #[async_trait]
    impl Tool for TerminalTool {
        fn name(&self) -> &str {
            "terminal"
        }
        fn description(&self) -> &str {
            "runs shell commands"
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

    struct SearchTool;

    #[async_trait]
    impl Tool for SearchTool {
        fn name(&self) -> &str {
            "search"
        }
        fn description(&self) -> &str {
            "searches text"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        fn output_category(&self) -> OutputCategory {
            OutputCategory::MatchList
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, "ok")
        }
    }

    struct FileTool;

    #[async_trait]
    impl Tool for FileTool {
        fn name(&self) -> &str {
            "file"
        }
        fn description(&self) -> &str {
            "reads files"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        fn output_category(&self) -> OutputCategory {
            OutputCategory::FileContent
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, "ok")
        }
    }

    #[test]
    fn register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "echo" });
        assert!(reg.get("echo").is_some());
    }

    #[test]
    fn get_unknown_returns_none() {
        let reg = ToolRegistry::new();
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn names_returns_all_registered() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "a" });
        reg.register(EchoTool { name: "b" });
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn schemas_contains_registered_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "my_tool" });
        let schemas = reg.schemas();
        assert!(schemas.iter().any(|s| s.name == "my_tool"));
    }

    #[test]
    fn schemas_include_description() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "t" });
        let schemas = reg.schemas();
        assert_eq!(schemas[0].description, "echoes its input");
    }

    #[tokio::test]
    async fn execute_known_tool_succeeds() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "echo" });
        let call = ToolCall {
            id: "1".into(),
            name: "echo".into(),
            args: json!({"x":1}),
        };
        let out = reg.execute(&call).await;
        assert!(!out.is_error);
        assert!(out.content.starts_with("echo:"));
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let reg = ToolRegistry::new();
        let call = ToolCall {
            id: "x".into(),
            name: "missing".into(),
            args: json!({}),
        };
        let out = reg.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown tool"));
    }

    #[test]
    fn registering_same_name_twice_overwrites() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "t" });
        reg.register(EchoTool { name: "t" });
        assert_eq!(reg.names().len(), 1);
    }

    // ── output_category ───────────────────────────────────────────────────────

    #[test]
    fn output_category_unknown_tool_returns_generic() {
        let reg = ToolRegistry::new();
        assert_eq!(reg.output_category("no_such_tool"), OutputCategory::Generic);
    }

    #[test]
    fn output_category_tool_without_override_returns_generic() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "echo" });
        assert_eq!(reg.output_category("echo"), OutputCategory::Generic);
    }

    #[test]
    fn output_category_headtail_tool_returns_headtail() {
        let mut reg = ToolRegistry::new();
        reg.register(TerminalTool);
        assert_eq!(reg.output_category("terminal"), OutputCategory::HeadTail);
    }

    #[test]
    fn output_category_matchlist_tool_returns_matchlist() {
        let mut reg = ToolRegistry::new();
        reg.register(SearchTool);
        assert_eq!(reg.output_category("search"), OutputCategory::MatchList);
    }

    #[test]
    fn output_category_filecontent_tool_returns_filecontent() {
        let mut reg = ToolRegistry::new();
        reg.register(FileTool);
        assert_eq!(reg.output_category("file"), OutputCategory::FileContent);
    }

    #[test]
    fn output_category_after_overwrite_reflects_new_tool() {
        // Register a HeadTail tool, then overwrite the same name with a Generic tool.
        let mut reg = ToolRegistry::new();
        reg.register(TerminalTool); // "terminal" → HeadTail
                                    // Overwrite with a minimal (Generic) tool under the same name.
        struct GenericTool;
        #[async_trait::async_trait]
        impl Tool for GenericTool {
            fn name(&self) -> &str {
                "terminal"
            }
            fn description(&self) -> &str {
                "generic"
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
        reg.register(GenericTool);
        assert_eq!(
            reg.output_category("terminal"),
            OutputCategory::Generic,
            "output_category must reflect the most recently registered tool"
        );
    }

    #[test]
    fn output_category_multiple_tools_independent() {
        let mut reg = ToolRegistry::new();
        reg.register(TerminalTool);
        reg.register(SearchTool);
        reg.register(FileTool);
        reg.register(EchoTool { name: "echo" });

        assert_eq!(reg.output_category("terminal"), OutputCategory::HeadTail);
        assert_eq!(reg.output_category("search"), OutputCategory::MatchList);
        assert_eq!(reg.output_category("file"), OutputCategory::FileContent);
        assert_eq!(reg.output_category("echo"), OutputCategory::Generic);
        assert_eq!(reg.output_category("missing"), OutputCategory::Generic);
    }
}
