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

/// A server of the current revision whose tools change while sessions run:
/// it announces each change on its listen streams, and can end them.
mod growing_server {
    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, ErrorData, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, ServerNotification,
        SubscriptionFilter, Tool, ToolListChangedNotification,
    };
    use rmcp::service::{RequestContext, RoleServer, SubscriptionContext};
    use std::sync::{Arc, Mutex};
    use tokio::sync::broadcast;

    /// What the test changes, shared with every session's handler.
    pub struct Shared {
        pub tools: Mutex<Vec<String>>,
        /// `true` announces a change; `false` ends every open stream.
        pub events: broadcast::Sender<bool>,
        pub listens: std::sync::atomic::AtomicUsize,
    }

    impl Shared {
        pub fn new(tools: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                tools: Mutex::new(tools.iter().map(|t| t.to_string()).collect()),
                events: broadcast::channel(16).0,
                listens: Default::default(),
            })
        }
    }

    #[derive(Clone)]
    pub struct Server(pub Arc<Shared>);

    impl ServerHandler for Server {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_tool_list_changed()
                    .build(),
            )
        }

        fn accepted_subscription_filter(
            &self,
            requested: &SubscriptionFilter,
        ) -> Option<SubscriptionFilter> {
            let mut accepted = SubscriptionFilter::new();
            accepted.tools_list_changed = requested.tools_list_changed;
            Some(accepted)
        }

        async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
            self.0
                .listens
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut events = self.0.events.subscribe();
            loop {
                tokio::select! {
                    () = context.cancelled() => return Ok(()),
                    event = events.recv() => match event {
                        Ok(true) => {
                            let changed = ServerNotification::ToolListChangedNotification(ToolListChangedNotification::default());
                            let _ = context.sink().send(changed).await;
                        }
                        // The stream ends, as a server's does at a restart
                        // or when its credential expires.
                        _ => return Ok(()),
                    },
                }
            }
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
            let names = self.0.tools.lock().unwrap().clone();
            Ok(ListToolsResult::with_all_items(
                names
                    .into_iter()
                    .map(|n| Tool::new(n, "a tool", schema.clone()))
                    .collect(),
            ))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            Err(ErrorData::invalid_params(
                format!("no call to {}", request.name),
                None,
            ))
        }
    }
}

/// The growing server over Streamable HTTP.
async fn serve_growing(shared: Arc<growing_server::Shared>) -> String {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::service::TowerToHyperService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

    let service = StreamableHttpService::new(
        move || Ok(growing_server::Server(shared.clone())),
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

/// Waits up to `secs` for `done`, checking every 50 ms.
async fn eventually<F: std::future::Future<Output = bool>>(
    secs: u64,
    mut done: impl FnMut() -> F,
) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if done().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// What the token-protected server saw.
#[derive(Default)]
struct Trades {
    traded: std::sync::atomic::AtomicUsize,
    key_seen_at_mcp: std::sync::atomic::AtomicBool,
}

/// The current server behind short-lived tokens: `/token` trades the key
/// `k` for a token, `/mcp` takes only the latest token, and refuses the
/// first one it is shown once, so a client must trade again.
async fn serve_with_tokens(trades: Arc<Trades>) -> (String, String) {
    use bytes::Bytes;
    use http_body_util::{combinators::BoxBody, BodyExt, Full};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
    use std::sync::atomic::Ordering::SeqCst;
    use tower_service::Service as _;

    let service = StreamableHttpService::new(
        || Ok(current_server::Server),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let refused_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (service, trades, refused_once) = (
                service.clone(),
                Arc::clone(&trades),
                Arc::clone(&refused_once),
            );
            let handle =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let (mut service, trades, refused_once) = (
                        service.clone(),
                        Arc::clone(&trades),
                        Arc::clone(&refused_once),
                    );
                    async move {
                        type Body = BoxBody<Bytes, std::convert::Infallible>;
                        let reply = |status: u16, text: String| -> hyper::Response<Body> {
                            hyper::Response::builder()
                                .status(status)
                                .header("content-type", "application/json")
                                .header("www-authenticate", "Bearer")
                                .body(Full::new(Bytes::from(text)).boxed())
                                .unwrap()
                        };
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        if req.uri().path() == "/token" {
                            if auth != "Bearer k" {
                                return Ok::<_, std::convert::Infallible>(reply(401, "{}".into()));
                            }
                            let n = trades.traded.fetch_add(1, SeqCst) + 1;
                            return Ok(reply(
                                200,
                                format!(r#"{{"access_token":"t{n}","expires_in":600}}"#),
                            ));
                        }
                        if auth == "Bearer k" {
                            trades.key_seen_at_mcp.store(true, SeqCst);
                        }
                        let latest = format!("Bearer t{}", trades.traded.load(SeqCst));
                        if auth != latest
                            || (auth == "Bearer t1" && !refused_once.swap(true, SeqCst))
                        {
                            return Ok(reply(401, "{}".into()));
                        }
                        Ok(service.call(req).await.unwrap())
                    }
                });
            tokio::spawn(async move {
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), handle)
                    .await;
            });
        }
    });
    (format!("{base}/mcp"), format!("{base}/token"))
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

/// How the OAuth server of [`serve_oauth`] answers a refresh.
#[derive(Clone, Copy)]
enum Refresh {
    /// Trades `good-refresh` for the access token `fresh`.
    Grants,
    /// Answers `invalid_grant`: the refresh token is revoked.
    Rejects,
    /// Answers 503: the server is down, the token may still be good.
    Down,
}

/// An OAuth-protected MCP server (its own authorization server too) that
/// takes only the access token `fresh`, which `/token` grants for the
/// refresh token `good-refresh` as `refresh` says. Returns its MCP URL.
async fn serve_oauth(refresh: Refresh) -> String {
    use bytes::Bytes;
    use http_body_util::{combinators::BoxBody, BodyExt, Full};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
    use tower_service::Service as _;

    let service = StreamableHttpService::new(
        || Ok(current_server::Server),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let origin = base.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (service, base) = (service.clone(), origin.clone());
            let handle = hyper::service::service_fn(
                move |req: hyper::Request<hyper::body::Incoming>| {
                    let (mut service, base) = (service.clone(), base.clone());
                    async move {
                        type Body = BoxBody<Bytes, std::convert::Infallible>;
                        let reply =
                            |status: u16, json: serde_json::Value| -> hyper::Response<Body> {
                                hyper::Response::builder()
                                    .status(status)
                                    .header("content-type", "application/json")
                                    .header("www-authenticate", "Bearer")
                                    .body(Full::new(Bytes::from(json.to_string())).boxed())
                                    .unwrap()
                            };
                        let path = req.uri().path().to_string();
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let answer = if path.starts_with("/.well-known/oauth-protected-resource") {
                            reply(
                                200,
                                json!({"resource": format!("{base}/mcp"), "authorization_servers": [base]}),
                            )
                        } else if path.starts_with("/.well-known/oauth-authorization-server") {
                            reply(
                                200,
                                json!({
                                    "issuer": base,
                                    "authorization_endpoint": format!("{base}/authorize"),
                                    "token_endpoint": format!("{base}/token"),
                                    "code_challenge_methods_supported": ["S256"],
                                    "response_types_supported": ["code"],
                                }),
                            )
                        } else if path == "/token" {
                            match refresh {
                                Refresh::Grants => reply(
                                    200,
                                    json!({"access_token": "fresh", "token_type": "Bearer",
                                           "expires_in": 3600, "refresh_token": "good-refresh"}),
                                ),
                                Refresh::Rejects => reply(400, json!({"error": "invalid_grant"})),
                                Refresh::Down => reply(503, json!({})),
                            }
                        } else if auth == "Bearer fresh" {
                            return Ok::<_, std::convert::Infallible>(
                                service.call(req).await.unwrap(),
                            );
                        } else {
                            reply(401, json!({}))
                        };
                        Ok(answer)
                    }
                },
            );
            tokio::spawn(async move {
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), handle)
                    .await;
            });
        }
    });
    format!("{base}/mcp")
}

/// A manager whose token store already holds a sign-in to the server at
/// `url`: the access token `stale` (with no recorded lifetime, so the
/// client cannot know it expired) and the refresh token `good-refresh`.
fn signed_in_client(url: &str) -> (Client, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp-oauth.json");
    let key = TokenStore::key("gate", url);
    let origin = url.trim_end_matches("/mcp");
    std::fs::write(
        &path,
        json!({ key.clone(): {
            "client_id": "registered",
            "token_response": {"access_token": "stale", "token_type": "bearer",
                               "refresh_token": "good-refresh"},
            "issuer": origin,
        }})
        .to_string(),
    )
    .unwrap();
    let (tx, events) = mpsc::channel(64);
    let manager = McpManager::with_token_store(
        [("gate".to_string(), http(url, 10))].into(),
        tx,
        false,
        TokenStore::new(path.clone()),
    );
    let client = Client {
        manager,
        events,
        _store: dir,
    };
    (client, path, key)
}

async fn an_access_token_the_server_refuses_is_refreshed_before_the_sign_in_is_forgotten() {
    let url = serve_oauth(Refresh::Grants).await;
    let (c, path, key) = signed_in_client(&url);
    c.manager.connect("gate").await.unwrap();
    assert_eq!(c.manager.tools().await.len(), 4);
    let kept = TokenStore::new(path).load(&key).unwrap().expect("kept");
    let token = serde_json::to_value(kept.token_response.unwrap()).unwrap();
    assert_eq!(token["access_token"], "fresh");
}

async fn a_revoked_refresh_token_forgets_the_sign_in_and_asks_for_a_new_one() {
    let url = serve_oauth(Refresh::Rejects).await;
    let (mut c, path, key) = signed_in_client(&url);
    assert!(c.manager.connect("gate").await.is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(c.events.recv().await, Some(McpEvent::AuthRequired { .. })) {}
    })
    .await
    .expect("AuthRequired in time");
    assert!(TokenStore::new(path).load(&key).unwrap().is_none());
}

async fn a_refresh_that_cannot_reach_the_server_keeps_the_sign_in() {
    let url = serve_oauth(Refresh::Down).await;
    let (mut c, path, key) = signed_in_client(&url);
    assert!(c.manager.connect("gate").await.is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match c.events.recv().await {
                Some(McpEvent::ServerFailed { .. }) => break,
                Some(McpEvent::AuthRequired { .. }) => {
                    panic!("a server outage asked for a sign-in")
                }
                Some(_) => {}
                None => panic!("no ServerFailed"),
            }
        }
    })
    .await
    .expect("ServerFailed in time");
    assert!(
        TokenStore::new(path).load(&key).unwrap().is_some(),
        "an outage forgot the sign-in"
    );
}

async fn a_sign_in_that_cannot_be_read_for_want_of_the_network_does_not_open_a_browser() {
    // The server answers 401 but its authorization server cannot be reached,
    // so the stored sign-in cannot be read: the server fails (and is tried
    // again later); the person is not treated as signed out and asked to
    // sign in.
    let url = serve_unauthorized().await;
    let (mut c, path, key) = signed_in_client(&url);
    assert!(c.manager.connect("gate").await.is_err());
    let mut failed = false;
    while let Ok(event) = c.events.try_recv() {
        match event {
            McpEvent::ServerFailed { .. } => failed = true,
            McpEvent::AuthStarted { .. } | McpEvent::AuthRequired { .. } => {
                panic!("an unreadable sign-in asked for a new one")
            }
            _ => {}
        }
    }
    assert!(failed, "the server was not reported failed");
    assert!(TokenStore::new(path).load(&key).unwrap().is_some());
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

async fn a_server_behind_short_lived_tokens_never_sees_the_key_and_is_traded_again_when_refused() {
    use std::sync::atomic::Ordering::SeqCst;
    let trades = Arc::new(Trades::default());
    let (url, token_url) = serve_with_tokens(Arc::clone(&trades)).await;
    let c = client(vec![(
        "gate",
        McpServerConfig::exchanged_http(&url, &token_url, "k"),
    )]);
    c.manager.connect("gate").await.unwrap();
    assert_eq!(c.manager.tools().await.len(), 4);
    let out = call(&c, "gate", "kinds").await.unwrap();
    assert_eq!(out.parts.len(), 5);
    assert!(
        trades.traded.load(SeqCst) >= 2,
        "the refused token was traded again"
    );
    assert!(
        !trades.key_seen_at_mcp.load(SeqCst),
        "the key went to the MCP endpoint"
    );
}

async fn a_servers_tool_policy_decides_what_is_offered_and_what_asks() {
    use sven_mcp_client::McpToolPolicy;
    use sven_tool_api::{ApprovalPolicy, Tool};
    let mut config = stdio("current", 10);
    config.tools = HashMap::from([
        ("*".to_string(), McpToolPolicy::Deny),
        ("kinds".to_string(), McpToolPolicy::Allow),
        ("structured".to_string(), McpToolPolicy::Ask),
    ]);
    let c = client(vec![("new", config)]);
    c.manager.connect("new").await.unwrap();
    let tools = c.manager.tools().await;
    let offered: Vec<(&str, ApprovalPolicy)> = tools
        .iter()
        .map(|t| (t.name(), t.default_policy()))
        .collect();
    assert_eq!(
        offered,
        vec![
            ("new-kinds", ApprovalPolicy::Auto),
            ("new-structured", ApprovalPolicy::Ask)
        ]
    );
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

async fn a_tool_the_server_adds_while_the_session_runs_reaches_it_without_a_restart() {
    let shared = growing_server::Shared::new(&["first"]);
    let url = serve_growing(shared.clone()).await;
    let c = client(vec![("live", http(&url, 10))]);
    c.manager.connect("live").await.unwrap();
    c.manager.start_background_tasks();
    assert_eq!(c.manager.tools().await.len(), 1);
    let generation = c.manager.tool_changes();
    let seen = *generation.borrow();
    // The session's listen stream is open before the change is made.
    assert!(
        eventually(5, || async {
            shared.listens.load(std::sync::atomic::Ordering::SeqCst) >= 1
        })
        .await
    );
    shared.tools.lock().unwrap().push("second".into());
    let _ = shared.events.send(true);
    assert!(
        eventually(5, || async { c.manager.tools().await.len() == 2 }).await,
        "the added tool reached the running session"
    );
    assert!(
        *generation.borrow() > seen,
        "the change was announced to the session"
    );
}

async fn a_change_made_while_the_stream_was_down_is_still_seen() {
    let shared = growing_server::Shared::new(&["first"]);
    let url = serve_growing(shared.clone()).await;
    let c = client(vec![("live", http(&url, 10))]);
    c.manager.connect("live").await.unwrap();
    c.manager.start_background_tasks();
    assert!(
        eventually(5, || async {
            shared.listens.load(std::sync::atomic::Ordering::SeqCst) >= 1
        })
        .await
    );
    // The stream ends, and the tool is added with nobody listening.
    let _ = shared.events.send(false);
    shared.tools.lock().unwrap().push("second".into());
    assert!(
        eventually(10, || async { c.manager.tools().await.len() == 2 }).await,
        "listening again lists again, so the missed change is seen"
    );
    assert!(shared.listens.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

async fn a_list_that_did_not_change_is_not_announced() {
    let shared = growing_server::Shared::new(&["first"]);
    let url = serve_growing(shared.clone()).await;
    let c = client(vec![("live", http(&url, 10))]);
    c.manager.connect("live").await.unwrap();
    c.manager.start_background_tasks();
    assert!(
        eventually(5, || async {
            shared.listens.load(std::sync::atomic::Ordering::SeqCst) >= 1
        })
        .await
    );
    let generation = c.manager.tool_changes();
    let seen = *generation.borrow();
    let _ = shared.events.send(true);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        *generation.borrow(),
        seen,
        "an announcement with nothing new leaves the session's tools alone"
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
            a_tool_the_server_adds_while_the_session_runs_reaches_it_without_a_restart,
            a_change_made_while_the_stream_was_down_is_still_seen,
            a_list_that_did_not_change_is_not_announced,
            a_servers_tool_policy_decides_what_is_offered_and_what_asks,
            a_server_behind_short_lived_tokens_never_sees_the_key_and_is_traded_again_when_refused,
            a_401_in_a_headless_run_asks_for_a_sign_in_and_never_waits,
            an_access_token_the_server_refuses_is_refreshed_before_the_sign_in_is_forgotten,
            a_revoked_refresh_token_forgets_the_sign_in_and_asks_for_a_new_one,
            a_refresh_that_cannot_reach_the_server_keeps_the_sign_in,
            a_sign_in_that_cannot_be_read_for_want_of_the_network_does_not_open_a_browser,
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
