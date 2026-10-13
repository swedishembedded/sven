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

use serde_json::Value;
use sven_chunks::{
    render_expansion, ChunkId, ExpansionRequest, Freshness, LineRange, SharedChunkStore,
    SourceProbe,
};
use sven_tool_api::tool::{ToolCall, ToolOutput};

/// Reads chunks back from the session's [`ChunkStore`](sven_chunks::ChunkStore).
pub struct ChunkExpander {
    store: SharedChunkStore,
    probe: Arc<dyn SourceProbe + Send + Sync>,
}

impl ChunkExpander {
    /// `probe` is confined to the session's root: it is how a file-derived
    /// chunk is checked for staleness, never a way to read a file.
    #[must_use]
    pub fn new(store: SharedChunkStore, probe: Arc<dyn SourceProbe + Send + Sync>) -> Self {
        Self { store, probe }
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

        let mut store = self
            .store
            .lock()
            .map_err(|_| "the chunk store is unavailable".to_string())?;
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
