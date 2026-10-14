// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A stub MCP server that offers a generated tool catalog.
//!
//! The catalog is the "many similar tools" of the tool-selection suite. The
//! server speaks newline-delimited JSON-RPC on stdin and stdout: `initialize`,
//! `tools/list` and `tools/call`. Every call answers with the tool's name, so
//! a call is visible in the trajectory and costs a fixed few tokens.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::task::CatalogTool;

/// The protocol version the stub answers `initialize` with.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// The response to one JSON-RPC message, or `None` for a notification.
#[must_use]
pub fn handle(catalog: &[CatalogTool], message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ablation-catalog", "version": "1"},
        }),
        "tools/list" => json!({
            "tools": catalog.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": {
                    "type": "object",
                    "properties": {"id": {"type": "string", "description": "Identifier of the record."}},
                },
            })).collect::<Vec<_>>(),
        }),
        "tools/call" => {
            let name = message
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if catalog.iter().all(|t| t.name != name) {
                return Some(json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32602, "message": format!("unknown tool {name}")},
                }));
            }
            json!({"content": [{"type": "text", "text": format!("ok: {name}")}], "isError": false})
        }
        "ping" => json!({}),
        _ => {
            return Some(json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("method not found: {method}")},
            }))
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Serves `catalog` until `input` ends.
///
/// # Errors
///
/// An I/O failure on either stream.
pub fn serve(
    catalog: &[CatalogTool],
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(response) = handle(catalog, &message) {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
    Ok(())
}
