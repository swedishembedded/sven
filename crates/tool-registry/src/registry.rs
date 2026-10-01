// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sven_config::AgentMode;
use sven_hsm::ToolCapability;

use sven_tool_api::policy::PermissionRequester;
use sven_tool_api::tool::ToolDisplayRegistry;
use sven_tool_api::{ApprovalPolicy, OutputCategory, Tool, ToolCall, ToolOutput};
pub use sven_vocab::ToolSchema;

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
/// Works exactly like [`sven_workspace::SharedSkills`] and
/// [`sven_workspace::SharedAgents`]: callers hold a cheap `Clone` and call
/// `.get()` to obtain an `Arc<[ToolSchema]>` snapshot without locking.
///
/// The store is populated by the runtime builder after the registry is built
/// so that the TUI can list available tools via `/tools` without reaching into
/// the kernel's internals.
pub type SharedTools = sven_workspace::Shared<ToolSchema>;

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
    /// `session/request_permission` round-trip to the IDE before executing,
    /// bounded by the requester's own timeout.
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
    /// `default_policy` is [`ApprovalPolicy::Ask`] waits for the requester's
    /// answer; the requester bounds that wait (ACP: the permission timeout).
    pub fn set_permission_requester(&mut self, requester: Arc<dyn PermissionRequester>) {
        self.permission_requester = Some(requester);
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        if let Ok(mut guard) = self.tools.write() {
            guard.insert(tool.name().to_string(), Arc::new(tool));
        }
    }

    /// Register an already-`Arc`-wrapped tool.
    ///
    /// [`register`](Self::register) and [`register_with_display`](Self::register_with_display)
    /// both require a concrete `impl Tool` so they can mint the `Arc`
    /// themselves; a caller that already holds a `Arc<dyn Tool>` - a test
    /// double substituted for a real tool, or a tool shared across more than
    /// one registry - has no way to hand it over without this. Mirrors how
    /// [`replace_mcp_tools`](Self::replace_mcp_tools) already stores
    /// `Arc<dyn Tool>` directly for MCP-sourced tools.
    pub fn register_arc(&mut self, tool: Arc<dyn Tool>) {
        if let Ok(mut guard) = self.tools.write() {
            guard.insert(tool.name().to_string(), tool);
        }
    }

    /// Register a tool that also provides display metadata. The same instance
    /// is used for execution and for TUI display (collapsed summary, display name).
    pub fn register_with_display(
        &mut self,
        tool: impl Tool + sven_tool_api::tool::ToolDisplay + 'static,
    ) {
        let arc = Arc::new(tool);
        let name = arc.name().to_string();
        if let Ok(mut guard) = self.tools.write() {
            guard.insert(name.clone(), Arc::clone(&arc) as Arc<dyn Tool>);
        }
        if let Ok(mut disp) = self.display_registry.write() {
            disp.register_arc(name, arc as Arc<dyn sven_tool_api::tool::ToolDisplay>);
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

    /// Removes the tool registered as `name`, so it is neither offered nor
    /// run. Returns whether one was registered.
    pub fn remove(&mut self, name: &str) -> bool {
        self.tools
            .write()
            .is_ok_and(|mut tools| tools.remove(name).is_some())
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

    /// Produce schemas only for the named subset of tools.
    ///
    /// Used by the SDLC deliberation engine to give each state a *state-scoped*
    /// tool subset (e.g. discovery may read/grep but not write).  Unknown names
    /// are silently skipped so a state can request a superset without erroring.
    /// Ordering follows `schemas_filtered`: core tools first (sorted), then
    /// MCP tools (sorted), preserving stable cache breakpoints.
    pub fn schemas_for_names(&self, names: &[String]) -> Vec<ToolSchema> {
        let allow: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
        self.schemas_filtered(|t| allow.contains(t.name()))
    }

    /// Of the requested `names`, return those that are actually registered.
    ///
    /// Lets a deliberation report (and a caller validate) which of its
    /// state-scoped tools are available in the current runtime.
    pub fn known_names<'a>(&self, names: &'a [String]) -> Vec<&'a str> {
        let guard = match self.tools.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        names
            .iter()
            .map(String::as_str)
            .filter(|n| guard.contains_key(*n))
            .collect()
    }

    /// Returns the [`ToolCapability`] for the named tool.
    ///
    /// Calls [`Tool::kernel_capability`] on the registered tool instance.
    /// Returns [`ToolCapability::NetworkAccess`] for unknown tools (MCP default
    /// and safe fallback so callers never silently grant narrower permissions).
    pub fn capability_of(&self, name: &str) -> ToolCapability {
        self.tools
            .read()
            .ok()
            .and_then(|g| g.get(name).map(|t| t.kernel_capability()))
            .unwrap_or(ToolCapability::NetworkAccess)
    }

    /// The capability `call` exercises ([`Tool::call_capability`]), with the
    /// same fallback for an unknown tool as [`Self::capability_of`].
    pub fn capability_of_call(&self, call: &ToolCall) -> ToolCapability {
        self.tools
            .read()
            .ok()
            .and_then(|g| g.get(&call.name).map(|t| t.call_capability(&call.args)))
            .unwrap_or(ToolCapability::NetworkAccess)
    }

    /// Runs `call`. A tool whose policy is [`ApprovalPolicy::Deny`] never
    /// runs; one whose policy is [`ApprovalPolicy::Ask`] is put to the
    /// registry's permission requester when a host set one (an IDE over ACP)
    /// and runs only if it allows it; otherwise the call runs. Whether a
    /// person approves a call in an agent session is the kernel's decision
    /// (the session's approval mode), not this one.
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
        match tool.default_policy() {
            ApprovalPolicy::Auto => {}
            ApprovalPolicy::Deny => {
                return ToolOutput::err(
                    &call.id,
                    format!("tool '{}' is denied by policy", call.name),
                );
            }
            ApprovalPolicy::Ask => {
                if let Some(requester) = &self.permission_requester {
                    let capability = tool.call_capability(&call.args);
                    if !requester.request_permission(call, capability).await {
                        return ToolOutput::err(
                            &call.id,
                            format!("tool '{}' was denied by the client", call.name),
                        );
                    }
                }
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
            .and_then(|g| g.get(tool_name).map(|t| t.output_category()))
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
    use sven_tool_api::policy::ApprovalPolicy;
    use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};

    /// Minimal no-op tool for registry tests.
    struct EchoTool {
        name: &'static str,
    }

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            self.name
        }
        fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
            sven_tool_api::ToolCapability::ReadFile
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
        fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
            sven_tool_api::ToolCapability::ReadFile
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
        fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
            sven_tool_api::ToolCapability::ReadFile
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
        fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
            sven_tool_api::ToolCapability::ReadFile
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
    fn register_arc_makes_a_pre_wrapped_tool_retrievable() {
        let mut reg = ToolRegistry::new();
        let tool: Arc<dyn Tool> = Arc::new(EchoTool { name: "echo" });
        reg.register_arc(Arc::clone(&tool));
        assert!(reg.get("echo").is_some());
    }

    #[test]
    fn register_arc_shares_the_same_instance_a_caller_still_holds() {
        // The point of accepting a pre-wrapped `Arc<dyn Tool>` (rather than
        // only `impl Tool + 'static`, like `register`) is that a caller can
        // keep its own handle to the exact same tool instance - e.g. a test
        // that asserts against a fake tool's internal state after a run.
        let mut reg = ToolRegistry::new();
        let tool: Arc<dyn Tool> = Arc::new(EchoTool { name: "echo" });
        reg.register_arc(Arc::clone(&tool));
        let fetched = reg
            .get("echo")
            .expect("registered tool must be retrievable");
        assert!(Arc::ptr_eq(&tool, &fetched));
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
    fn schemas_for_names_returns_only_requested_subset() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "a" });
        reg.register(EchoTool { name: "b" });
        reg.register(EchoTool { name: "c" });
        let subset = reg.schemas_for_names(&["a".to_string(), "c".to_string()]);
        let mut names: Vec<&str> = subset.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn schemas_for_names_skips_unknown() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "a" });
        let subset = reg.schemas_for_names(&["a".to_string(), "missing".to_string()]);
        assert_eq!(subset.len(), 1);
        assert_eq!(subset[0].name, "a");
    }

    #[test]
    fn known_names_filters_to_registered() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool { name: "a" });
        reg.register(EchoTool { name: "b" });
        let req = vec!["a".to_string(), "x".to_string(), "b".to_string()];
        let mut known = reg.known_names(&req);
        known.sort_unstable();
        assert_eq!(known, vec!["a", "b"]);
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

    // ── Tool policy: `Deny` never runs, a host decides `Ask` ──────────────────

    /// Tool whose policy is `Ask`: the host's requester decides it, if set.
    struct AskTool;

    #[async_trait]
    impl Tool for AskTool {
        fn name(&self) -> &str {
            "ask_tool"
        }
        fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
            sven_tool_api::ToolCapability::ReadFile
        }
        fn description(&self) -> &str {
            "requires approval"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Ask
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, "executed")
        }
    }

    /// Tool whose policy is `Deny` - must never run at all.
    struct DenyTool;

    #[async_trait]
    impl Tool for DenyTool {
        fn name(&self) -> &str {
            "deny_tool"
        }
        fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
            sven_tool_api::ToolCapability::ReadFile
        }
        fn description(&self) -> &str {
            "always denied"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Deny
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, "executed")
        }
    }

    /// Requester that always answers with a fixed decision, and checks it
    /// is told what the call does.
    struct FixedRequester(bool);

    #[async_trait]
    impl sven_tool_api::policy::PermissionRequester for FixedRequester {
        async fn request_permission(
            &self,
            _call: &ToolCall,
            capability: sven_tool_api::ToolCapability,
        ) -> bool {
            assert_eq!(capability, sven_tool_api::ToolCapability::ReadFile);
            self.0
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            args: json!({}),
        }
    }

    /// With no host asking, an `Ask` tool runs: approval in an agent
    /// session is the kernel's decision.
    #[tokio::test]
    async fn an_ask_tool_runs_when_no_host_asks() {
        let mut reg = ToolRegistry::new();
        reg.register(AskTool);
        let out = reg.execute(&call("ask_tool")).await;
        assert!(!out.is_error, "{}", out.content);
    }

    #[tokio::test]
    async fn a_host_requester_decides_an_ask_tool() {
        for allow in [true, false] {
            let mut reg = ToolRegistry::new();
            reg.register(AskTool);
            reg.set_permission_requester(Arc::new(FixedRequester(allow)));
            let out = reg.execute(&call("ask_tool")).await;
            assert_eq!(out.is_error, !allow, "{}", out.content);
        }
    }

    #[tokio::test]
    async fn a_deny_tool_never_runs() {
        let mut reg = ToolRegistry::new();
        reg.register(DenyTool);
        reg.set_permission_requester(Arc::new(FixedRequester(true)));
        let out = reg.execute(&call("deny_tool")).await;
        assert!(out.is_error && out.content.contains("denied by policy"));
    }

    #[tokio::test]
    async fn an_unknown_tool_returns_an_error() {
        let reg = ToolRegistry::new();
        let out = reg.execute(&call("missing")).await;
        assert!(out.is_error && out.content.contains("unknown tool"));
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
            fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
                sven_tool_api::ToolCapability::ReadFile
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
