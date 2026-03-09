// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compound `context` tool that consolidates all 5 RLM context tools into a
//! single action-dispatched interface.
//!
//! Replaces: context_open, context_read, context_grep, context_query, context_reduce
//!
//! The context tools implement the Recursive Language Model (RLM) pattern for
//! analyzing content too large to fit in the model's context window.
//! Consolidating them saves 4 tool definitions from the tools array while
//! keeping the full capability intact.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

use sven_config::{AgentMode, Config};
use sven_model::ModelProvider;
use sven_tools::{
    events::ToolEvent,
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
    ContextGrepTool, ContextOpenTool, ContextReadTool, ContextStore,
};

use crate::context_query::{build_context_query_tools, ContextQueryTool, ContextReduceTool};

/// Compound context tool — all RLM large-content tools in one definition.
pub struct ContextTool {
    open: ContextOpenTool,
    read: ContextReadTool,
    grep: ContextGrepTool,
    query: ContextQueryTool,
    reduce: ContextReduceTool,
}

impl ContextTool {
    pub fn new(
        store: Arc<Mutex<ContextStore>>,
        model: Arc<dyn ModelProvider>,
        cfg: &Config,
        progress_tx: Option<mpsc::Sender<ToolEvent>>,
    ) -> Self {
        let (query, reduce) = build_context_query_tools(store.clone(), model, cfg, progress_tx);
        Self {
            open: ContextOpenTool::new(store.clone()),
            read: ContextReadTool::new(store.clone()),
            grep: ContextGrepTool::new(store),
            query,
            reduce,
        }
    }
}

#[async_trait]
impl Tool for ContextTool {
    fn name(&self) -> &str {
        "context"
    }

    fn description(&self) -> &str {
        "Memory-mapped analysis for content too large for the context window.\n\
         action: open | read | grep | query | reduce\n\n\
         Workflow: open → grep (locate) → read (inspect) → query (map) → reduce (synthesize)\n\
         - open: load a file or directory into a handle; content stays OUT of context\n\
         - read: random-access line range from a handle (use after grep to narrow)\n\
         - grep: regex search over a handle; cheap pre-filter before read or query\n\
         - query: dispatch analysis prompt to sub-agents over chunks; returns new handle\n\
         - reduce: synthesize/aggregate a results handle into a final answer\n\n\
         Use for: files >500 lines, build logs, large codebases, binary analysis."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["open", "read", "grep", "query", "reduce"],
                    "description": "Which context operation to perform"
                },
                "path": {
                    "type": "string",
                    "description": "[action=open] File or directory path to open"
                },
                "handle": {
                    "type": "string",
                    "description": "[action=read|grep|query|reduce] Context handle from a previous open or query"
                },
                "start_line": {
                    "type": "integer",
                    "description": "[action=read] First line to read (1-indexed)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "[action=read] Last line to read (inclusive)"
                },
                "file": {
                    "type": "string",
                    "description": "[action=read] File path within a directory handle"
                },
                "pattern": {
                    "type": "string",
                    "description": "[action=grep] Regex pattern to search for"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "[action=grep] Lines of context before/after each match (default 0)"
                },
                "limit": {
                    "type": "integer",
                    "description": "[action=grep] Max matches to return (default 50)"
                },
                "prompt": {
                    "type": "string",
                    "description": "[action=query|reduce] Analysis or synthesis prompt"
                },
                "ranges": {
                    "type": "array",
                    "description": "[action=query] Optional line ranges to query; omit to process entire context",
                    "items": {
                        "type": "object",
                        "properties": {
                            "start_line": {"type": "integer"},
                            "end_line": {"type": "integer"},
                            "file": {"type": "string"}
                        },
                        "required": ["start_line", "end_line"]
                    }
                },
                "chunk_lines": {
                    "type": "integer",
                    "description": "[action=query] Lines per chunk (default: configured default)"
                },
                "max_parallel": {
                    "type": "integer",
                    "description": "[action=query] Max concurrent sub-queries (default: configured default)"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'action'"),
        };

        match action.as_str() {
            "open" => {
                let path = match call.args.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => {
                        return ToolOutput::err(
                            &call.id,
                            "missing required parameter 'path' for action=open",
                        )
                    }
                };
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "context_open".into(),
                    args: json!({ "path": path }),
                };
                self.open.execute(&delegate_call).await
            }
            "read" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "context_read".into(),
                    args,
                };
                self.read.execute(&delegate_call).await
            }
            "grep" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "context_grep".into(),
                    args,
                };
                self.grep.execute(&delegate_call).await
            }
            "query" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "context_query".into(),
                    args,
                };
                self.query.execute(&delegate_call).await
            }
            "reduce" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "context_reduce".into(),
                    args,
                };
                self.reduce.execute(&delegate_call).await
            }
            other => ToolOutput::err(
                &call.id,
                format!(
                    "unknown action '{}'. Valid: open, read, grep, query, reduce",
                    other
                ),
            ),
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use sven_tools::tool::ToolCall;

    // ContextTool requires a model provider for query/reduce; we only test
    // the routing logic and the actions that don't need a live model.

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "context".into(),
            args,
        }
    }

    fn make_tool() -> ContextTool {
        use std::sync::Arc;
        use sven_config::Config;
        use sven_model::MockProvider;
        use sven_tools::ContextStore;
        use tokio::sync::Mutex;

        let store = Arc::new(Mutex::new(ContextStore::new()));
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider);
        let cfg = Config::default();
        ContextTool::new(store, provider, &cfg, None)
    }

    #[tokio::test]
    async fn missing_action_is_error() {
        let t = make_tool();
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'action'"));
    }

    #[tokio::test]
    async fn unknown_action_is_error() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "fly"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown action"));
    }

    #[tokio::test]
    async fn open_missing_path_is_error() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "open"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("path"));
    }
}
