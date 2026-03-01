// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! End-to-end integration tests for the sven MCP server.
//!
//! Each test drives a real [`SvenMcpServer`] over in-memory pipes, sending
//! raw JSON-RPC 2.0 messages and validating the responses.  This exercises
//! the full rmcp dispatch path and confirms that the sven ↔ MCP bridge
//! behaves correctly from a client's perspective.
//!
//! The helpers in this file intentionally use raw JSON instead of an rmcp
//! client so that tests are independent of the rmcp client API and directly
//! verify the wire format that real MCP hosts will see.

use std::sync::Arc;

use async_trait::async_trait;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use sven_mcp::SvenMcpServer;
use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput, ToolRegistry};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, WriteHalf};

// ── Test tool fixtures ────────────────────────────────────────────────────────

/// A minimal echo tool: returns the `message` argument or "no message".
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes the message argument back to the caller"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        })
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let msg = call
            .args
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        ToolOutput::ok(&call.id, msg)
    }
}

/// A tool that always returns an error result.
struct AlwaysFailTool;

#[async_trait]
impl Tool for AlwaysFailTool {
    fn name(&self) -> &str {
        "always_fail"
    }
    fn description(&self) -> &str {
        "Always returns an error"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::err(&call.id, "this tool always fails")
    }
}

// ── In-process MCP server harness ────────────────────────────────────────────

/// Starts a [`SvenMcpServer`] in a background task connected to in-memory
/// pipes.  Returns a writer (to send JSON-RPC to the server) and a buffered
/// reader (to read JSON-RPC responses from the server).
///
/// Uses a pair of `tokio::io::duplex` streams:
/// - `client_stream`: the client end — write here to send to the server,
///   read here to get server responses.
/// - `server_stream`: passed directly to the server (DuplexStream implements
///   both AsyncRead and AsyncWrite).
async fn start_test_server(
    registry: Arc<ToolRegistry>,
) -> (
    WriteHalf<DuplexStream>,
    BufReader<tokio::io::ReadHalf<DuplexStream>>,
) {
    // tokio::io::duplex creates two connected halves.  Writes on one end
    // appear as reads on the other end.
    let (client_stream, server_stream) = tokio::io::duplex(65536);

    tokio::spawn(async move {
        let server = SvenMcpServer::new(registry);
        if let Ok(running) = server.serve(server_stream).await {
            let _ = running.waiting().await;
        }
    });

    // Split the client stream so we can use BufReader for line-oriented reads.
    let (client_read, client_write) = tokio::io::split(client_stream);
    let reader = BufReader::new(client_read);
    (client_write, reader)
}

/// Write a JSON-RPC message as a single newline-terminated line.
async fn send_msg(writer: &mut WriteHalf<DuplexStream>, msg: &Value) {
    let line = serde_json::to_string(msg).expect("message must serialize");
    writer
        .write_all(line.as_bytes())
        .await
        .expect("write failed");
    writer.write_all(b"\n").await.expect("newline write failed");
    writer.flush().await.expect("flush failed");
}

/// Read one JSON-RPC response line from the server.  Times out after 5 s.
async fn recv_msg(reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>) -> Value {
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for server response")
    .expect("read error");
    serde_json::from_str(line.trim()).expect("server response must be valid JSON")
}

/// Send the MCP `initialize` handshake and drain the matching response plus
/// the `notifications/initialized` notification.  Returns the `initialize`
/// result object.
async fn initialize(
    writer: &mut WriteHalf<DuplexStream>,
    reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>,
) -> Value {
    send_msg(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "sven-test-client", "version": "0.0.0" }
            }
        }),
    )
    .await;

    // The server sends back the initialize result.
    let init_resp = recv_msg(reader).await;
    assert_eq!(
        init_resp["jsonrpc"], "2.0",
        "initialize response must be JSON-RPC 2.0"
    );
    assert!(
        init_resp["result"].is_object(),
        "initialize must return a result object"
    );

    // After receiving the result the client must send `initialized`.
    send_msg(
        writer,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;

    init_resp["result"].clone()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The MCP `initialize` handshake completes and declares tool support.
#[tokio::test]
async fn initialize_declares_tools_capability() {
    let reg = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        r
    });
    let (mut writer, mut reader) = start_test_server(reg).await;
    let result = initialize(&mut writer, &mut reader).await;
    assert!(
        result["capabilities"]["tools"].is_object(),
        "server must advertise tools capability; got: {result}"
    );
}

/// `tools/list` returns the registered tools with correct name and description.
#[tokio::test]
async fn tools_list_returns_registered_tools() {
    let reg = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        r
    });
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools must be an array");
    assert_eq!(tools.len(), 1, "expected exactly 1 tool");
    assert_eq!(tools[0]["name"], "echo");
    assert_eq!(
        tools[0]["description"],
        "Echoes the message argument back to the caller"
    );
}

/// `tools/list` with an empty registry returns an empty tools array.
#[tokio::test]
async fn tools_list_empty_registry() {
    let reg = Arc::new(ToolRegistry::new());
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools must be an array");
    assert!(tools.is_empty(), "expected no tools in empty registry");
}

/// `tools/list` includes the JSON Schema in the `inputSchema` field.
#[tokio::test]
async fn tools_list_includes_input_schema() {
    let reg = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        r
    });
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    let tools = &resp["result"]["tools"];
    let schema = &tools[0]["inputSchema"];
    assert_eq!(
        schema["type"], "object",
        "inputSchema must have type:object"
    );
    assert!(
        schema["properties"]["message"].is_object(),
        "schema must include message property"
    );
}

/// A successful `tools/call` returns the tool output with `isError: false`.
#[tokio::test]
async fn tools_call_success_returns_content() {
    let reg = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        r
    });
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "echo",
                "arguments": { "message": "hello from test" }
            }
        }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    assert!(
        resp["result"].is_object(),
        "call must return a result; got: {resp}"
    );
    assert_eq!(resp["result"]["isError"], false);

    let content = resp["result"]["content"]
        .as_array()
        .expect("content must be an array");
    assert!(!content.is_empty(), "content must not be empty");
    assert_eq!(content[0]["text"], "hello from test");
}

/// A tool that returns an error sets `isError: true` in the response.
#[tokio::test]
async fn tools_call_error_tool_sets_is_error() {
    let reg = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(AlwaysFailTool);
        r
    });
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "always_fail", "arguments": {} }
        }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    assert_eq!(
        resp["result"]["isError"], true,
        "always_fail must set isError:true; got {resp}"
    );

    let content = resp["result"]["content"]
        .as_array()
        .expect("content must be an array");
    assert!(!content.is_empty());
    assert!(
        content[0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("always fails"),
        "error message must be forwarded"
    );
}

/// Calling an unknown tool returns a result with `isError: true` (not a
/// JSON-RPC error).  sven's ToolRegistry wraps the "unknown tool" case in a
/// ToolOutput::err, so the MCP layer sees a tool-level error, not a protocol
/// error.
#[tokio::test]
async fn tools_call_unknown_tool_returns_is_error() {
    let reg = Arc::new(ToolRegistry::new());
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "nonexistent", "arguments": {} }
        }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    // The server either returns isError:true or a JSON-RPC error — both are acceptable.
    let is_tool_error = resp["result"]["isError"] == true;
    let is_rpc_error = resp["error"].is_object();
    assert!(
        is_tool_error || is_rpc_error,
        "unknown tool must produce an error; got: {resp}"
    );
}

/// Multiple tools can be listed and called independently.
#[tokio::test]
async fn tools_call_multiple_tools_independently() {
    let reg = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        r.register(AlwaysFailTool);
        r
    });
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    // List both tools
    send_msg(
        &mut writer,
        &json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": {} }),
    )
    .await;
    let list_resp = recv_msg(&mut reader).await;
    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert_eq!(tools.len(), 2, "both tools must be listed");

    // Call echo successfully
    send_msg(
        &mut writer,
        &json!({
            "jsonrpc": "2.0", "id": 6,
            "method": "tools/call",
            "params": { "name": "echo", "arguments": { "message": "ping" } }
        }),
    )
    .await;
    let echo_resp = recv_msg(&mut reader).await;
    assert_eq!(echo_resp["result"]["isError"], false);

    // Call always_fail
    send_msg(
        &mut writer,
        &json!({
            "jsonrpc": "2.0", "id": 7,
            "method": "tools/call",
            "params": { "name": "always_fail", "arguments": {} }
        }),
    )
    .await;
    let fail_resp = recv_msg(&mut reader).await;
    assert_eq!(fail_resp["result"]["isError"], true);
}

/// The `build_mcp_registry` helper registers the default safe tool set.
/// This test verifies the plumbing between registry and server without
/// actually executing the tools.
#[tokio::test]
async fn default_registry_tools_are_listed_by_server() {
    let reg = Arc::new(sven_mcp::build_mcp_registry(None, None));
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": {} }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    let tools = resp["result"]["tools"].as_array().expect("tools array");

    // At minimum the core read/write tools must be present.
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"read_file"),
        "read_file must be listed; got: {names:?}"
    );
    assert!(
        names.contains(&"write_file"),
        "write_file must be listed; got: {names:?}"
    );
    assert!(
        names.contains(&"grep"),
        "grep must be listed; got: {names:?}"
    );
    assert!(
        names.contains(&"run_terminal_command"),
        "run_terminal_command must be listed"
    );
}

/// Filtered registry only exposes the requested tools.
#[tokio::test]
async fn filtered_registry_limits_exposed_tools() {
    let reg = Arc::new(sven_mcp::build_mcp_registry(None, Some("read_file,grep")));
    let (mut writer, mut reader) = start_test_server(reg).await;
    initialize(&mut writer, &mut reader).await;

    send_msg(
        &mut writer,
        &json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": {} }),
    )
    .await;

    let resp = recv_msg(&mut reader).await;
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert_eq!(
        tools.len(),
        2,
        "filtered registry must expose exactly 2 tools"
    );

    let names: std::collections::HashSet<&str> =
        tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains("read_file"));
    assert!(names.contains("grep"));
}
