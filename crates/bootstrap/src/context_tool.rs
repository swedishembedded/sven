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

use crate::context_query::ContextConfig;
use sven_model::ModelProvider;
use sven_tool_api::{
    events::ToolEvent,
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
};
use sven_tools_ctx::{
    ChunkExpander, ContextGrepTool, ContextOpenTool, ContextReadTool, ContextStore,
};
use sven_turn::TurnLimits;
use sven_vocab::AgentMode;

use crate::context_query::{build_context_query_tools, ContextQueryTool, ContextReduceTool};

/// Compound context tool - all RLM large-content tools in one definition.
pub struct ContextTool {
    open: ContextOpenTool,
    read: ContextReadTool,
    grep: ContextGrepTool,
    query: ContextQueryTool,
    reduce: ContextReduceTool,
    /// Present when demoted tool results can be expanded (see
    /// [`Self::with_expansion`]).
    expand: Option<ChunkExpander>,
}

/// Modes that offer the tool without expansion.
const BASE_MODES: &[AgentMode] = &[AgentMode::Agent];
/// With expansion the tool is offered wherever a result can be demoted, so a
/// reference is never a dead end. Its other actions stay under the permission
/// policy of each mode.
const EXPANDING_MODES: &[AgentMode] = &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent];

impl ContextTool {
    pub fn new(
        store: Arc<Mutex<ContextStore>>,
        model: Arc<dyn ModelProvider>,
        cfg: &ContextConfig,
        limits: TurnLimits,
        progress_tx: Option<mpsc::Sender<ToolEvent>>,
    ) -> Self {
        let (query, reduce) =
            build_context_query_tools(store.clone(), model, cfg, limits, progress_tx);
        Self {
            open: ContextOpenTool::new(store.clone()),
            read: ContextReadTool::new(store.clone()),
            grep: ContextGrepTool::new(store),
            query,
            reduce,
            expand: None,
        }
    }

    /// Enables `action=expand`, which returns the raw text behind a reference
    /// in a demoted tool result. Until then the tool's definition, modes and
    /// actions are exactly what they are without addressable context.
    #[must_use]
    pub fn with_expansion(mut self, expander: ChunkExpander) -> Self {
        self.expand = Some(expander);
        self
    }

    /// Opens only paths `scope` allows, as the file tools do.
    #[must_use]
    pub fn with_scope(mut self, scope: sven_tool_api::PathScope) -> Self {
        self.open = self.open.with_scope(scope);
        self
    }
}

#[async_trait]
impl Tool for ContextTool {
    fn name(&self) -> &str {
        "context"
    }

    fn summary(&self) -> &str {
        "Analyse content too large for the prompt through a memory-mapped context: open, read, grep."
    }

    fn description(&self) -> &str {
        if self.expand.is_some() {
            return "Memory-mapped analysis for content too large for the context window.\n\
         action: open | read | grep | query | reduce | expand\n\n\
         Workflow: open → grep (locate) → read (inspect) → query (map) → reduce (synthesize)\n\
         - open: load a file or directory into a handle; content stays OUT of context\n\
         - read: random-access line range from a handle (use after grep to narrow)\n\
         - grep: regex search over a handle; cheap pre-filter before read or query\n\
         - query: dispatch analysis prompt to sub-agents over chunks; returns new handle\n\
         - reduce: synthesize/aggregate a results handle into a final answer\n\
         - expand: return the raw text of a chunk (c-...) that an earlier tool result showed \
         in a shorter form; optional first_line/last_line. Says when the source file changed.\n\n\
         Use for: files >500 lines, build logs, large codebases, binary analysis.";
        }
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
        let mut schema = json!({
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
        });
        if self.expand.is_some() {
            schema["properties"]["action"]["enum"]
                .as_array_mut()
                .expect("action is an enum")
                .push(json!("expand"));
            let properties = schema["properties"]
                .as_object_mut()
                .expect("properties is an object");
            properties.insert(
                "chunk".into(),
                json!({
                    "type": "string",
                    "description": "[action=expand] Chunk id (c-...) shown in a shortened tool result"
                }),
            );
            properties.insert(
                "first_line".into(),
                json!({
                    "type": "integer",
                    "description": "[action=expand] First line of the chunk to return (1-indexed, default 1)"
                }),
            );
            properties.insert(
                "last_line".into(),
                json!({
                    "type": "integer",
                    "description": "[action=expand] Last line of the chunk to return (inclusive, default the end)"
                }),
            );
        }
        schema
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    /// `query` and `reduce` send the content to the model provider; the
    /// rest only read it.
    fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
        sven_tool_api::ToolCapability::NetworkAccess
    }

    fn call_capability(&self, args: &Value) -> sven_tool_api::ToolCapability {
        use sven_tool_api::ToolCapability::{NetworkAccess, ReadFile};
        sven_tool_api::capability_by_action(
            args,
            &[
                ("open", ReadFile),
                ("read", ReadFile),
                ("grep", ReadFile),
                ("query", NetworkAccess),
                ("reduce", NetworkAccess),
                ("expand", ReadFile),
            ],
            NetworkAccess,
        )
    }

    fn modes(&self) -> &[AgentMode] {
        if self.expand.is_some() {
            EXPANDING_MODES
        } else {
            BASE_MODES
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'action'"),
        };

        if let (Some(expander), "expand") = (&self.expand, action.as_str()) {
            return expander.expand(call);
        }

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
                let mut open_args = json!({ "path": path });
                if let Some(pat) = call.args.get("include_pattern") {
                    open_args["include_pattern"] = pat.clone();
                }
                if let Some(rec) = call.args.get("recursive") {
                    open_args["recursive"] = rec.clone();
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "context_open".into(),
                    args: open_args,
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
    use sven_tool_api::tool::ToolCall;

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
        use sven_model_mock::MockProvider;
        use sven_tools_ctx::ContextStore;
        use tokio::sync::Mutex;

        let store = Arc::new(Mutex::new(ContextStore::new()));
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider);
        ContextTool::new(
            store,
            provider,
            &ContextConfig::default(),
            sven_turn::TurnLimits::default(),
            None,
        )
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

    fn expanding_tool() -> (ContextTool, String, tempfile::TempDir) {
        use sven_chunks::{
            ChunkDraft, ChunkKind, ChunkStore, FsProbe, Lifetime, Provenance, Sensitivity,
            SourceProbe, StoreLimits,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut chunks = ChunkStore::open(dir.path(), StoreLimits::default()).unwrap();
        let id = chunks
            .put(
                "alpha\nbeta\n",
                ChunkDraft {
                    kind: ChunkKind::CommandOutput,
                    provenance: Provenance {
                        source_tool: "shell".into(),
                        args_digest: String::new(),
                        path: None,
                        source_version: None,
                        turn: 1,
                    },
                    priority: 0,
                    sensitivity: Sensitivity::Internal,
                    lifetime: Lifetime::Session,
                    pinned: false,
                    failure: false,
                },
            )
            .unwrap()
            .chunk
            .id
            .to_string();
        let probe: Arc<dyn SourceProbe + Send + Sync> = Arc::new(FsProbe::new(dir.path()).unwrap());
        let expander =
            sven_tools_ctx::ChunkExpander::new(Arc::new(std::sync::Mutex::new(chunks)), probe);
        (make_tool().with_expansion(expander), id, dir)
    }

    #[test]
    fn expansion_is_not_advertised_unless_it_is_enabled() {
        let t = make_tool();
        let schema = t.parameters_schema();
        assert!(!schema.to_string().contains("expand"));
        assert!(schema["properties"].get("chunk").is_none());
        assert!(!t.description().contains("expand"));
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn expand_is_an_unknown_action_when_it_is_not_enabled() {
        let out = make_tool()
            .execute(&call(
                json!({"action": "expand", "chunk": "c-0123456789abcdef"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown action"));
    }

    #[tokio::test]
    async fn an_enabled_expansion_returns_the_chunk_and_is_read_only() {
        let (t, id, _dir) = expanding_tool();
        let schema = t.parameters_schema();
        assert!(schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("expand")));
        assert!(schema["properties"].get("chunk").is_some());
        assert!(t.modes().contains(&AgentMode::Plan) && t.modes().contains(&AgentMode::Research));
        assert_eq!(
            t.call_capability(&json!({"action": "expand"})),
            sven_tool_api::ToolCapability::ReadFile
        );
        let out = t
            .execute(&call(json!({"action": "expand", "chunk": id})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.ends_with("\nalpha\nbeta\n"));
    }
}
