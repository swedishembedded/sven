// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that work with every
// tool server they meet for its clients. If your team needs expertise in
// Model Context Protocol interoperability then you can procure our services
// by sending an email to info@swedishembedded.com.
//! sven's MCP client against real servers of both protocol eras, over both
//! transports, through the manager the agent uses.
//!
//! The stdio servers are this test binary itself, started again with
//! `--fixture <name>` naming the server to be: a legacy one (`rmcp` 0.15,
//! `initialize` only) and a current one (`rmcp` 3.8, `server/discover`). So
//! the suite has its own `main` (`harness = false`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use sven_mcp_client::{McpEvent, McpManager, McpServerConfig, McpTransport, TokenStore};
use tokio::sync::mpsc;

const FIXTURE: &str = "--fixture";

// ── The servers ─────────────────────────────────────────────────────────────

/// `initialize`-only server: six tools over three pages, one of them slow.
mod legacy_server {
    use std::sync::Arc;
    use std::time::Duration;

    use legacy::handler::server::ServerHandler;
    use legacy::model::{
        CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerInfo, Tool,
    };
    use legacy::service::{RequestContext, RoleServer};
    use legacy::{ErrorData, ServiceExt};

    #[derive(Clone)]
    struct Server;

    impl ServerHandler for Server {
        fn get_info(&self) -> ServerInfo {
            ServerInfo {
                capabilities: ServerCapabilities::builder().enable_tools().build(),
                ..ServerInfo::default()
            }
        }

        async fn list_tools(
            &self,
            request: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let page: usize = request
                .and_then(|r| r.cursor)
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let schema = Arc::new(serde_json::Map::from_iter([(
                "type".to_string(),
                serde_json::json!("object"),
            )]));
            let tools = (0..2)
                .map(|i| Tool::new(format!("t{}", page * 2 + i), "a tool", schema.clone()))
                .collect();
            Ok(ListToolsResult {
                tools,
                next_cursor: (page < 2).then(|| (page + 1).to_string()),
                meta: None,
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _: RequestContext<RoleServer>,
        ) -> Result<CallToolResult, ErrorData> {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(CallToolResult::success(vec![Content::text(format!(
                "ran {}",
                request.name
            ))]))
        }
    }

    pub async fn run() {
        // A legacy server closes on a request it does not know before
        // `initialize` (the probe this suite expects); it ends quietly.
        if let Ok(service) = Server.serve(legacy::transport::stdio()).await {
            let _ = service.waiting().await;
        }
    }
}

/// A server of the current revision: every content type, a structured-only
/// result, a call that never ends and one that kills the server.
mod current_server {
    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool,
    };
    use rmcp::service::{RequestContext, RoleServer};
    use rmcp::{ErrorData, ServiceExt};
    use std::sync::Arc;

    #[derive(Clone)]
    pub struct Server;

    impl ServerHandler for Server {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn list_tools(
            &self,
            _: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let schema = Arc::new(serde_json::Map::from_iter([(
                "type".to_string(),
                serde_json::json!("object"),
            )]));
            let tools = ["kinds", "structured", "hang", "die"]
                .into_iter()
                .map(|name| Tool::new(name, "a tool", schema.clone()))
                .collect();
            Ok(ListToolsResult::with_all_items(tools))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let result = match request.name.as_ref() {
                "kinds" => CallToolResult::success(vec![
                    ContentBlock::text("words"),
                    ContentBlock::image("aW1n", "image/png"),
                    ContentBlock::audio("YXVk", "audio/wav"),
                    ContentBlock::resource(ResourceContents::text("inside", "file:///a.txt")),
                    ContentBlock::resource_link(Resource::new("file:///b.pdf", "b.pdf")),
                ]),
                "structured" => serde_json::from_value(serde_json::json!({
                    "content": [],
                    "structuredContent": {"temperature": 21}
                }))
                .unwrap(),
                "hang" => {
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    unreachable!()
                }
                "die" => std::process::exit(3),
                other => return Err(ErrorData::invalid_params(format!("no tool {other}"), None)),
            };
            Ok(CallToolResponse::Complete(result))
        }
    }

    pub async fn run() {
        let service = Server.serve(rmcp::transport::stdio()).await.unwrap();
        let _ = service.waiting().await;
    }
}

/// The current server over Streamable HTTP, at `http://127.0.0.1:<port>/mcp`.
async fn serve_http() -> String {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::service::TowerToHyperService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

    let service = StreamableHttpService::new(
        || Ok(current_server::Server),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let service = TowerToHyperService::new(service.clone());
            tokio::spawn(async move {
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    url
}

/// A server that refuses everything with a 401 and its challenge.
async fn serve_unauthorized() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf).await;
            let answer = "HTTP/1.1 401 Unauthorized\r\n\
                www-authenticate: Bearer resource_metadata=\"http://127.0.0.1:1/.well-known/oauth-protected-resource\"\r\n\
                content-length: 0\r\nconnection: close\r\n\r\n";
            let _ = socket.write_all(answer.as_bytes()).await;
        }
    });
    url
}

// ── The client side ─────────────────────────────────────────────────────────

struct Client {
    manager: Arc<McpManager>,
    events: mpsc::Receiver<McpEvent>,
    _store: tempfile::TempDir,
}

fn stdio(fixture: &str, timeout_secs: u64) -> McpServerConfig {
    McpServerConfig {
        transport: McpTransport::Stdio {
            command: std::env::current_exe().unwrap().display().to_string(),
            args: vec![FIXTURE.to_string(), fixture.to_string()],
        },
        timeout_secs,
        ..McpServerConfig::default()
    }
}

fn http(url: &str, timeout_secs: u64) -> McpServerConfig {
    McpServerConfig {
        transport: McpTransport::Http {
            url: url.to_string(),
            headers: HashMap::new(),
        },
        timeout_secs,
        ..McpServerConfig::default()
    }
}

/// A headless manager (no browser) with a token store of its own.
fn client(servers: Vec<(&str, McpServerConfig)>) -> Client {
    let dir = tempfile::tempdir().unwrap();
    let (tx, events) = mpsc::channel(64);
    let manager = McpManager::with_token_store(
        servers
            .into_iter()
            .map(|(n, c)| (n.to_string(), c))
            .collect(),
        tx,
        false,
        TokenStore::new(dir.path().join("mcp-oauth.json")),
    );
    Client {
        manager,
        events,
        _store: dir,
    }
}

fn text(output: &sven_tool_api::ToolOutput) -> String {
    output.content.clone()
}

async fn call(c: &Client, server: &str, tool: &str) -> anyhow::Result<sven_tool_api::ToolOutput> {
    let result = c.manager.call_tool(server, tool, &json!({})).await?;
    Ok(sven_mcp_client::bridge::tool_output("call", result))
}

// ── The cases ───────────────────────────────────────────────────────────────

async fn a_legacy_stdio_server_is_started_again_with_initialize_and_paged_to_the_end() {
    let c = client(vec![("old", stdio("legacy", 10))]);
    c.manager.connect("old").await.unwrap();
    let mut names: Vec<String> = c
        .manager
        .tools()
        .await
        .iter()
        .map(|t| sven_tool_api::Tool::name(t).to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["old-t0", "old-t1", "old-t2", "old-t3", "old-t4", "old-t5"]
    );
    let out = call(&c, "old", "t3").await.unwrap();
    assert_eq!(text(&out), "ran t3");
}

async fn concurrent_calls_on_one_stdio_server_run_side_by_side() {
    let c = client(vec![("old", stdio("legacy", 10))]);
    c.manager.connect("old").await.unwrap();
    let started = Instant::now();
    let calls: Vec<_> = (0..4)
        .map(|i| {
            let manager = Arc::clone(&c.manager);
            tokio::spawn(
                async move { manager.call_tool("old", &format!("t{i}"), &json!({})).await },
            )
        })
        .collect();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    // Four 300 ms calls one after another would take 1.2 s.
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
}

async fn every_content_type_reaches_the_agent() {
    let c = client(vec![("new", stdio("current", 10))]);
    c.manager.connect("new").await.unwrap();
    let out = call(&c, "new", "kinds").await.unwrap();
    use sven_tool_api::ToolOutputPart as P;
    assert!(matches!(&out.parts[0], P::Text(t) if t == "words"));
    assert!(matches!(&out.parts[1], P::Image(i) if i == "data:image/png;base64,aW1n"));
    assert!(matches!(&out.parts[2], P::Audio(a) if a == "data:audio/wav;base64,YXVk"));
    assert!(
        matches!(&out.parts[3], P::Text(t) if t.contains("file:///a.txt") && t.contains("inside"))
    );
    assert!(matches!(&out.parts[4], P::Text(t) if t.contains("file:///b.pdf")));
    let out = call(&c, "new", "structured").await.unwrap();
    assert_eq!(text(&out), r#"{"temperature":21}"#);
}

async fn a_call_the_server_never_answers_ends_at_the_timeout() {
    let c = client(vec![("new", stdio("current", 1))]);
    c.manager.connect("new").await.unwrap();
    let started = Instant::now();
    let err = call(&c, "new", "hang").await.unwrap_err();
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

async fn a_server_that_dies_mid_call_fails_the_call_at_once() {
    let c = client(vec![("new", stdio("current", 30))]);
    c.manager.connect("new").await.unwrap();
    let started = Instant::now();
    assert!(call(&c, "new", "die").await.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

async fn a_streamable_http_server_of_the_current_revision_lists_and_calls() {
    let url = serve_http().await;
    let c = client(vec![("web", http(&url, 10))]);
    c.manager.connect("web").await.unwrap();
    assert_eq!(c.manager.tools().await.len(), 4);
    let out = call(&c, "web", "kinds").await.unwrap();
    assert_eq!(out.parts.len(), 5);
}

async fn a_401_in_a_headless_run_asks_for_a_sign_in_and_never_waits() {
    let url = serve_unauthorized().await;
    let mut c = client(vec![("locked", http(&url, 10))]);
    assert!(c.manager.connect("locked").await.is_err());
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match c.events.recv().await {
                Some(McpEvent::AuthRequired { server, .. }) => break server,
                Some(_) => continue,
                None => panic!("no AuthRequired"),
            }
        }
    })
    .await
    .expect("AuthRequired in time");
    assert_eq!(event, "locked");
    let status = &c.manager.server_statuses().await[0];
    assert!(
        matches!(
            status.status,
            sven_mcp_client::ServerStatus::NeedsAuth { .. }
        ),
        "{:?}",
        status.status
    );
}

// ── The runner ──────────────────────────────────────────────────────────────

macro_rules! cases {
    ($($case:ident),* $(,)?) => {
        vec![$((stringify!($case), Box::pin($case()) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>)),*]
    };
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == FIXTURE) {
        return match args.get(i + 1).map(String::as_str) {
            Some("legacy") => runtime.block_on(legacy_server::run()),
            Some("current") => runtime.block_on(current_server::run()),
            other => panic!("unknown fixture {other:?}"),
        };
    }
    let filter = args.into_iter().find(|a| !a.starts_with('-'));
    let (mut passed, mut failed) = (0, 0);
    let all = runtime.block_on(async {
        cases![
            a_legacy_stdio_server_is_started_again_with_initialize_and_paged_to_the_end,
            concurrent_calls_on_one_stdio_server_run_side_by_side,
            every_content_type_reaches_the_agent,
            a_call_the_server_never_answers_ends_at_the_timeout,
            a_server_that_dies_mid_call_fails_the_call_at_once,
            a_streamable_http_server_of_the_current_revision_lists_and_calls,
            a_401_in_a_headless_run_asks_for_a_sign_in_and_never_waits,
        ]
    });
    for (name, case) in all {
        if filter.as_deref().is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let outcome = runtime.block_on(async { tokio::spawn(case).await });
        match outcome {
            Ok(()) => {
                passed += 1;
                println!("test {name} ... ok");
            }
            Err(e) => {
                failed += 1;
                println!("test {name} ... FAILED: {e}");
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" },
        passed
    );
    if failed > 0 {
        std::process::exit(1);
    }
}
