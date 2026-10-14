// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The expansion action of the `context` tool: return the raw text behind a
//! reference that a smaller representation carried.
//!
//! An expansion is a recovery event. It is counted in the token ledger from
//! the call and its result (see `sven_vocab::ContextRecovery`), so a bad
//! demotion is visible as the expansions it caused.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sven_chunks::{
    render_expansion, ChunkId, ExpansionRequest, Freshness, LineRange, SharedChunkStore,
    SourceProbe, EXPAND_ACTION, EXPAND_TOOL_NAME,
};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_vocab::AgentMode;

/// Reads chunks back from the session's [`ChunkStore`](sven_chunks::ChunkStore).
#[derive(Clone)]
pub struct ChunkExpander {
    store: SharedChunkStore,
    probe: Arc<dyn SourceProbe + Send + Sync>,
    /// Evidence another session recorded, read only: consulted for an id the
    /// session's own store does not hold.
    snapshot: Option<SharedChunkStore>,
}

impl ChunkExpander {
    /// `probe` is confined to the session's root: it is how a file-derived
    /// chunk is checked for staleness, never a way to read a file.
    #[must_use]
    pub fn new(store: SharedChunkStore, probe: Arc<dyn SourceProbe + Send + Sync>) -> Self {
        Self {
            store,
            probe,
            snapshot: None,
        }
    }

    /// Also expands the chunks of `snapshot`, a store opened read-only (see
    /// [`ChunkStore::open_read_only`](sven_chunks::ChunkStore::open_read_only)),
    /// for ids the session's own store lacks. Staleness is judged by the same
    /// probe, so a snapshot chunk of a file that has since changed is
    /// reported stale, never as current.
    #[must_use]
    pub fn with_snapshot(mut self, snapshot: SharedChunkStore) -> Self {
        self.snapshot = Some(snapshot);
        self
    }

    /// The parameters of `action=expand`, for a tool definition that offers it.
    #[must_use]
    pub fn parameters() -> Map<String, Value> {
        let Value::Object(map) = json!({
            "chunk": {
                "type": "string",
                "description": "[action=expand] Chunk id (c-...) shown in a shortened tool result"
            },
            "first_line": {
                "type": "integer",
                "description": "[action=expand] First line of the chunk to return (1-indexed, default 1)"
            },
            "last_line": {
                "type": "integer",
                "description": "[action=expand] Last line of the chunk to return (inclusive, default the end)"
            }
        }) else {
            unreachable!("a JSON object literal is an object")
        };
        map
    }

    /// Run `action=expand`: `chunk`, and optionally `first_line` and
    /// `last_line` (1-based, inclusive).
    #[must_use]
    pub fn expand(&self, call: &ToolCall) -> ToolOutput {
        match self.try_expand(&call.args) {
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(message) => ToolOutput::err(&call.id, message),
        }
    }

    fn try_expand(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("chunk")
            .and_then(Value::as_str)
            .ok_or("missing required parameter 'chunk' for action=expand")
            .and_then(|c| {
                ChunkId::parse(c).map_err(|_| "'chunk' is not a chunk id (c- and 16 hex digits)")
            })?;
        let first = line_argument(args, "first_line")?;
        let last = line_argument(args, "last_line")?;

        let own = self
            .store
            .lock()
            .map_err(|_| "the chunk store is unavailable".to_string())?;
        let mut store = match (&self.snapshot, own.get(&id).is_some()) {
            (Some(snapshot), false) => {
                drop(own);
                snapshot
                    .lock()
                    .map_err(|_| "the chunk store is unavailable".to_string())?
            }
            _ => own,
        };
        let chunk = store
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("no chunk {id} in this session"))?;
        let raw = store.read(&id).map_err(|e| format!("{e:#}"))?;
        // A probe failure (a path outside the root) must not look like
        // freshness, and must not turn the expansion into an error.
        let freshness = store
            .freshness(&id, self.probe.as_ref())
            .unwrap_or(Freshness::Unverifiable);
        drop(store);

        let total_lines = raw.lines().count();
        let range = match (first, last) {
            (None, None) => None,
            (first, last) => {
                let first = first.unwrap_or(1);
                let last = last.unwrap_or(total_lines).min(total_lines);
                if first > total_lines {
                    return Err(format!(
                        "first_line {first} is past the end: {id} has {total_lines} lines"
                    ));
                }
                if last < first {
                    return Err(format!("last_line {last} is before first_line {first}"));
                }
                Some(LineRange { first, last })
            }
        };
        let text = match range {
            None => raw,
            Some(r) => lines(&raw, r),
        };
        Ok(render_expansion(&ExpansionRequest {
            id,
            kind: chunk.kind,
            total_lines,
            range,
            freshness: &freshness,
            text: &text,
        }))
    }
}

fn lines(raw: &str, range: LineRange) -> String {
    raw.split_inclusive('\n')
        .skip(range.first - 1)
        .take(range.last - range.first + 1)
        .collect()
}

fn line_argument(args: &Value, key: &str) -> Result<Option<usize>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be an integer >= 1")),
    }
}

/// The `context` tool in a session that has none of the large-content
/// actions: it offers only `expand`, so a reference in a shortened tool result
/// is never a dead end. Where the full `context` tool is registered it carries
/// this action itself and this tool is not registered.
pub struct ChunkExpandTool {
    expander: ChunkExpander,
}

impl ChunkExpandTool {
    #[must_use]
    pub fn new(expander: ChunkExpander) -> Self {
        Self { expander }
    }
}

#[async_trait]
impl Tool for ChunkExpandTool {
    fn name(&self) -> &str {
        EXPAND_TOOL_NAME
    }

    fn summary(&self) -> &str {
        "Return the raw text of a chunk that a shortened tool result referred to."
    }

    fn description(&self) -> &str {
        "Return the raw text behind a chunk id (c-...) that an earlier tool result showed in a \
         shorter form. action=expand, chunk=<id>; optionally first_line and last_line to read \
         only part of it. The first line of the result says which chunk and lines these are and \
         whether the file it was read from has changed since; a STALE chunk is what the file \
         contained then, not now."
    }

    fn parameters_schema(&self) -> Value {
        let mut properties = ChunkExpander::parameters();
        properties.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": [EXPAND_ACTION],
                "description": "Which context operation to perform"
            }),
        );
        json!({
            "type": "object",
            "properties": properties,
            "required": ["action", "chunk"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match call.args.get("action").and_then(Value::as_str) {
            Some(EXPAND_ACTION) => self.expander.expand(call),
            _ => ToolOutput::err(&call.id, format!("action must be '{EXPAND_ACTION}'")),
        }
    }
}
