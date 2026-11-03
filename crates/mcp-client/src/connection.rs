// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for connecting agents to tool
// servers of any protocol revision for its clients. If your team needs
// expertise in Model Context Protocol integration then you can procure our
// services by sending an email to info@swedishembedded.com.
//! One live connection to an MCP server, over stdio or Streamable HTTP, on
//! the `rmcp` client.
//!
//! The protocol revision is negotiated: a server of the 2026-07-28 revision
//! is spoken to with `server/discover` and per-request metadata, an older one
//! with `initialize`. Over HTTP the probe and the fallback share one
//! connection; a legacy stdio server closes its pipe on an unknown first
//! request, so it is started again and spoken to with `initialize`.
//!
//! A [`Connection`] is cheap to clone (it holds the peer handle only), so the
//! manager hands out clones and never holds its own lock across a call.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig,
    GetPromptRequestParams, Implementation, JsonObject, Prompt, ProtocolVersion, Tool,
};
use rmcp::service::{ClientInitializeError, NotificationContext, RunningService};
use rmcp::transport::auth::AuthClient;
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt, Peer, RoleClient};
use tokio::sync::mpsc;
use tracing::debug;

/// What a server asked to be refetched, by its `list_changed` notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListChanged {
    Tools,
    Prompts,
}

/// The server answered 401: it needs a sign-in, or the stored one was
/// refused. Carries the `WWW-Authenticate` challenge for discovery.
#[derive(Debug, Clone)]
pub struct AuthRequired {
    pub www_authenticate: Option<String>,
}

impl std::fmt::Display for AuthRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the MCP server requires authentication")
    }
}

impl std::error::Error for AuthRequired {}

/// Answers the server's notifications by telling the manager what changed.
#[derive(Clone)]
struct Handler {
    server: String,
    changed: mpsc::Sender<(String, ListChanged)>,
}

impl Handler {
    fn notify(&self, what: ListChanged) {
        // A full channel means a refresh is already queued; dropping this one
        // loses nothing.
        let _ = self.changed.try_send((self.server.clone(), what));
    }
}

impl ClientHandler for Handler {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("sven", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.notify(ListChanged::Tools);
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.notify(ListChanged::Prompts);
    }
}

/// The running service: dropping the last clone of it ends the connection
/// (and, for stdio, the server process).
type Service = RunningService<RoleClient, Handler>;

/// A live connection. Every request is bounded by the server's timeout.
#[derive(Clone)]
pub struct Connection {
    service: Arc<Service>,
    timeout: Duration,
}

/// How to reach one server.
pub enum Endpoint<'a> {
    Stdio {
        command: &'a str,
        args: &'a [String],
        env: &'a HashMap<String, String>,
    },
    Http {
        url: &'a str,
        headers: &'a HashMap<String, String>,
        /// Signs requests and refreshes the token; `None` for a server
        /// without OAuth (a static bearer token travels in `headers`).
        auth: Option<AuthClient<reqwest::Client>>,
        /// Short-lived tokens traded for a key, for every request.
        exchange: Option<crate::TokenExchange>,
    },
}

fn lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: None,
    }
}

impl Connection {
    /// Connect to the server named `server` and finish the protocol startup,
    /// within `timeout`. Its `list_changed` notifications go to `changed`.
    pub async fn open(
        server: &str,
        endpoint: Endpoint<'_>,
        timeout: Duration,
        changed: mpsc::Sender<(String, ListChanged)>,
    ) -> Result<Self> {
        let handler = Handler {
            server: server.to_string(),
            changed,
        };
        let service = match endpoint {
            Endpoint::Stdio { command, args, env } => {
                let first = within(
                    timeout,
                    handler
                        .clone()
                        .serve_with_lifecycle(spawn(server, command, args, env)?, lifecycle()),
                )
                .await;
                match first {
                    Ok(service) => service,
                    Err(e) => {
                        debug!(server, error = %format!("{e:#}"), "stdio server refused server/discover; starting it again with initialize");
                        within(
                            timeout,
                            handler.serve_with_lifecycle(
                                spawn(server, command, args, env)?,
                                ClientLifecycleMode::Initialize,
                            ),
                        )
                        .await
                        .map_err(|e| anyhow!("starting `{command}`: {}", e.root_cause()))?
                    }
                }
            }
            Endpoint::Http {
                url,
                headers,
                auth,
                exchange,
            } => {
                let config = http_config(url, headers)?;
                let serve = |mode: ClientLifecycleMode| {
                    serve_http(
                        handler.clone(),
                        config.clone(),
                        auth.clone(),
                        exchange.clone(),
                        mode,
                        timeout,
                    )
                };
                // Discovery that is not answered in time falls back to
                // `initialize` on the same transport, where the late answer
                // to discovery is then taken for the answer to `initialize`
                // and the startup fails. A server slow to answer its first
                // requests is therefore started again with `initialize` on a
                // transport of its own, as a stdio server that refused
                // discovery is. Only a failed startup is: a sign-in it asks
                // for is not retried, nor one that never answered at all.
                let result = match serve(lifecycle()).await {
                    Err(e)
                        if e.downcast_ref::<ClientInitializeError>().is_some()
                            && auth_required(&e).is_none() =>
                    {
                        debug!(server, error = %format!("{e:#}"), "server/discover failed over HTTP; connecting again with initialize");
                        serve(ClientLifecycleMode::Initialize).await
                    }
                    first => first,
                };
                // A sign-in it asks for stays typed for the manager; anything
                // else is said as what went wrong, not as the transport's
                // own wrapping of it.
                result.map_err(|e| {
                    auth_required(&e).map_or_else(
                        || anyhow!("cannot reach {url}: {}", e.root_cause()),
                        anyhow::Error::new,
                    )
                })?
            }
        };
        Ok(Self {
            service: Arc::new(service),
            timeout,
        })
    }

    fn peer(&self) -> &Peer<RoleClient> {
        self.service.peer()
    }

    /// Every tool, following the server's pages to the end.
    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        within(self.timeout, self.peer().list_all_tools())
            .await
            .context("tools/list")
    }

    /// Every prompt, following the server's pages to the end. A server
    /// without prompts has none.
    pub async fn list_prompts(&self) -> Result<Vec<Prompt>> {
        if self
            .peer()
            .peer_info()
            .is_some_and(|info| info.capabilities.prompts.is_none())
        {
            return Ok(Vec::new());
        }
        within(self.timeout, self.peer().list_all_prompts())
            .await
            .context("prompts/list")
    }

    /// Call `tool` with `args` (a JSON object, or nothing).
    pub async fn call_tool(&self, tool: &str, args: &serde_json::Value) -> Result<CallToolResult> {
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(object) = as_object(args) {
            params = params.with_arguments(object);
        }
        within(self.timeout, self.peer().call_tool(params))
            .await
            .with_context(|| format!("tools/call {tool}"))
    }

    /// The text of prompt `name` filled with `args`; non-text parts are
    /// named, not dropped.
    pub async fn get_prompt(&self, name: &str, args: &HashMap<String, String>) -> Result<String> {
        let arguments: JsonObject = args
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let params = GetPromptRequestParams::new(name).with_arguments(arguments);
        let result = within(self.timeout, self.peer().get_prompt(params))
            .await
            .with_context(|| format!("prompts/get {name}"))?;
        Ok(result
            .messages
            .iter()
            .map(|m| match m.content.as_text() {
                Some(text) => text.text.clone(),
                None => "[non-text prompt content omitted]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
}

/// Run `future` within `timeout`, so a server that goes quiet fails the
/// request instead of hanging the session.
/// Connects `handler` over Streamable HTTP with `config`, signing requests
/// with `auth` or trading `exchange` for short-lived tokens, and finishes the
/// protocol startup `mode` within `timeout`.
async fn serve_http(
    handler: Handler,
    config: StreamableHttpClientTransportConfig,
    auth: Option<AuthClient<reqwest::Client>>,
    exchange: Option<crate::TokenExchange>,
    mode: ClientLifecycleMode,
    timeout: Duration,
) -> Result<RunningService<RoleClient, Handler>> {
    match (auth, exchange) {
        (_, Some(exchange)) => {
            let transport = StreamableHttpClientTransport::with_client(
                crate::exchange::ExchangingClient::new(exchange),
                config,
            );
            within(timeout, handler.serve_with_lifecycle(transport, mode)).await
        }
        (Some(client), None) => {
            let transport = StreamableHttpClientTransport::with_client(client, config);
            within(timeout, handler.serve_with_lifecycle(transport, mode)).await
        }
        (None, None) => {
            let transport =
                StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
            within(timeout, handler.serve_with_lifecycle(transport, mode)).await
        }
    }
}

async fn within<T, E>(
    timeout: Duration,
    future: impl std::future::Future<Output = std::result::Result<T, E>>,
) -> Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result.map_err(anyhow::Error::new),
        Err(_) => Err(anyhow!("timed out after {}s", timeout.as_secs())),
    }
}

/// The server process, with a clean environment: only `PATH` and the
/// variables its configuration names, so sven's own secrets never reach it.
/// What it writes to stderr goes to sven's log as that server's, never to
/// the terminal, where it would draw over a TUI.
fn spawn(
    server: &str,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<TokioChildProcess> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args).env_clear().kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    cmd.envs(env);
    let (process, stderr) = TokioChildProcess::builder(cmd)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn MCP server: {command}"))?;
    if let Some(stderr) = stderr {
        let server = server.to_string();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                debug!(server = %server, "{line}");
            }
        });
    }
    Ok(process)
}

fn http_config(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<StreamableHttpClientTransportConfig> {
    let mut custom = HashMap::new();
    let mut bearer = None;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("authorization") {
            // The transport sends the authorization header itself.
            bearer = Some(value.strip_prefix("Bearer ").unwrap_or(value).to_string());
            continue;
        }
        let name = reqwest::header::HeaderName::try_from(name.as_str())
            .with_context(|| format!("header name `{name}`"))?;
        let value = reqwest::header::HeaderValue::try_from(value.as_str())
            .with_context(|| format!("value of header `{name}`"))?;
        custom.insert(name, value);
    }
    let mut config = StreamableHttpClientTransportConfig::with_uri(url).custom_headers(custom);
    if let Some(token) = bearer {
        config = config.auth_header(token);
    }
    Ok(config)
}

fn as_object(args: &serde_json::Value) -> Option<JsonObject> {
    match args {
        serde_json::Value::Object(map) => Some(map.clone()),
        _ => None,
    }
}

/// The 401 behind `error`, if the server refused for want of a sign-in.
///
/// `rmcp` reports a startup that failed in the transport as
/// [`ClientInitializeError::TransportError`], whose transport error is not
/// its `source()`, so that one is opened by hand before walking the chain.
fn auth_required(error: &anyhow::Error) -> Option<AuthRequired> {
    let found = |cause: &(dyn std::error::Error + 'static)| {
        let mut next = Some(cause);
        while let Some(e) = next {
            if let Some(e) = e.downcast_ref::<AuthRequiredError>() {
                return Some(AuthRequired {
                    www_authenticate: Some(e.www_authenticate_header.clone())
                        .filter(|h| !h.is_empty()),
                });
            }
            next = e.source();
        }
        None
    };
    error.chain().find_map(
        |cause| match cause.downcast_ref::<ClientInitializeError>() {
            Some(ClientInitializeError::TransportError { error, .. }) => {
                found(error.error.as_ref())
            }
            _ => found(cause),
        },
    )
}
