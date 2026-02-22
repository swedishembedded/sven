// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::sync::Arc;

use sven_config::AgentMode;

use crate::{Tool, ToolCall, ToolOutput};

/// A tool schema – mirrors sven_model::ToolSchema but keeps tools crate
/// independent from the model crate.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Central registry holding all available tools.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

// SAFETY: ToolRegistry is Sync because:
// - HashMap<String, Arc<dyn Tool>> is Sync (String is Sync, Arc<T: Send + Sync> is Sync)
// - Tools implement Send + Sync (required by the Tool trait)
// - No interior mutability exists after construction (all methods take &self)
// - Parallel tool execution is safe because tools are immutable after registration
unsafe impl Sync for ToolRegistry {}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name().to_string(), Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Produce schemas for ALL registered tools (mode-unfiltered).
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas: Vec<ToolSchema> = self.tools.values().map(|t| ToolSchema {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
        }).collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }

    /// Produce schemas only for tools available in the given mode.
    pub fn schemas_for_mode(&self, mode: AgentMode) -> Vec<ToolSchema> {
        let mut schemas: Vec<ToolSchema> = self.tools.values()
            .filter(|t| t.modes().contains(&mode))
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }

    pub async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match self.tools.get(&call.name) {
            Some(tool) => tool.execute(call).await,
            None => ToolOutput::err(
                &call.id,
                format!("unknown tool: {}", call.name),
            ),
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn names_for_mode(&self, mode: AgentMode) -> Vec<String> {
        let mut names: Vec<String> = self.tools.values()
            .filter(|t| t.modes().contains(&mode))
            .map(|t| t.name().to_string())
            .collect();
        names.sort();
        names
    }
}

impl Default for ToolRegistry {
    fn default() -> Self { Self::new() }
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
    struct EchoTool { name: &'static str }

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { self.name }
        fn description(&self) -> &str { "echoes its input" }
        fn parameters_schema(&self) -> Value { json!({ "type": "object" }) }
        fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, format!("echo:{}", call.args))
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
        let call = ToolCall { id: "1".into(), name: "echo".into(), args: json!({"x":1}) };
        let out = reg.execute(&call).await;
        assert!(!out.is_error);
        assert!(out.content.starts_with("echo:"));
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let reg = ToolRegistry::new();
        let call = ToolCall { id: "x".into(), name: "missing".into(), args: json!({}) };
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
}
