// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! MCP transport implementations (stdio subprocess and HTTP POST).
//!
//! Both transports implement a single `send_request` method that sends a
//! JSON-RPC 2.0 request and waits for the response.  Notifications
//! (initialize confirmation) are fire-and-forget via `send_notification`.
//!
//! ## HTTP Streamable Transport
//!
//! The HTTP transport implements the MCP Streamable HTTP spec:
//! - Sends `Accept: application/json, text/event-stream` per spec.
//! - Handles both plain JSON and `text/event-stream` responses.
//! - Captures and re-sends `mcp-session-id` for stateful server sessions.
//! - Proactively refreshes OAuth tokens within the refresh window.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tracing::{debug, trace, warn};

use crate::oauth::{refresh_token, CredentialsStore, StoredTokens};
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

/// Timeout applied to each MCP request/response exchange.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// ── UnauthorizedError ─────────────────────────────────────────────────────────

/// Error returned when an MCP HTTP server responds with HTTP 401.
///
/// Carries the raw `WWW-Authenticate` header value so callers can run the full
/// MCP OAuth discovery chain (RFC 9728 / RFC 8414) without any extra round trip.
#[derive(Debug)]
pub struct UnauthorizedError {
    /// The MCP server URL that rejected the request.
    pub url: String,
    /// Raw `WWW-Authenticate` response header, if the server sent one.
    pub www_authenticate: Option<String>,
}

impl std::fmt::Display for UnauthorizedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MCP server requires authentication (HTTP 401): {}",
            self.url
        )
    }
}

impl std::error::Error for UnauthorizedError {}

// ── StdioTransport ────────────────────────────────────────────────────────────

/// MCP transport over a stdio subprocess.
pub struct StdioTransport {
    _child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl StdioTransport {
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        timeout_secs: u64,
    ) -> Result<Self> {
        use tokio::process::Command;

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);

        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn MCP server: {command}"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        let timeout = if timeout_secs == 0 {
            DEFAULT_REQUEST_TIMEOUT
        } else {
            Duration::from_secs(timeout_secs)
        };

        Ok(Self {
            _child: child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicU64::new(1),
            timeout,
        })
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub async fn send_notification(&self, notif: &JsonRpcNotification) -> Result<()> {
        let mut line = serde_json::to_string(notif)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .context("write notification to MCP server stdin")?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn send_request(&self, req: &JsonRpcRequest) -> Result<Value> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');

        trace!(method = %req.method, id = req.id, "MCP → server");

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .context("write request to MCP server stdin")?;
            stdin.flush().await?;
        }

        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "MCP request {} timed out after {:?}",
                    req.method,
                    self.timeout
                ));
            }

            let remaining = deadline - tokio::time::Instant::now();
            let mut response_line = String::new();
            {
                let mut stdout = self.stdout.lock().await;
                let read_fut = stdout.read_line(&mut response_line);
                match tokio::time::timeout(remaining, read_fut).await {
                    Ok(Ok(0)) => {
                        return Err(anyhow!("MCP server closed stdout (EOF)"));
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        return Err(anyhow!("MCP server read error: {e}"));
                    }
                    Err(_) => {
                        return Err(anyhow!("MCP request {} timed out", req.method));
                    }
                }
            }

            let trimmed = response_line.trim();
            if trimmed.is_empty() {
                continue;
            }

            trace!(line = %trimmed, "MCP ← server");

            let resp: JsonRpcResponse = match serde_json::from_str(trimmed) {
                Ok(r) => r,
                Err(e) => {
                    debug!(line = %trimmed, error = %e, "skipping non-response line");
                    continue;
                }
            };

            let matches = match &resp.id {
                Some(Value::Number(n)) => n.as_u64() == Some(req.id),
                Some(Value::String(s)) => s.parse::<u64>().ok() == Some(req.id),
                _ => false,
            };

            if !matches {
                debug!(got_id = ?resp.id, expected = req.id, "id mismatch, skipping");
                continue;
            }

            if let Some(err) = resp.error {
                return Err(anyhow!("{}", err));
            }

            return resp
                .result
                .ok_or_else(|| anyhow!("MCP response missing result"));
        }
    }
}

// ── HttpTransport ─────────────────────────────────────────────────────────────

/// MCP transport over HTTP (Streamable HTTP POST).
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    next_id: AtomicU64,
    timeout: Duration,
    auth: Arc<Mutex<Option<AuthState>>>,
    /// Optional context for proactive token refresh.
    refresh_ctx: Option<RefreshContext>,
    /// Session ID from `mcp-session-id` response header.
    ///
    /// Stateful MCP servers (e.g. Atlassian) return this header on the
    /// `initialize` response and require it in all subsequent requests.
    session_id: Arc<Mutex<Option<String>>>,
}

/// Authentication state for an HTTP MCP server.
#[derive(Debug, Clone)]
pub enum AuthState {
    /// A static bearer token (from config headers).
    BearerToken(String),
    /// OAuth access token with full metadata for proactive refresh.
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<u64>,
        /// Token endpoint URL, needed for proactive refresh.
        token_endpoint: String,
        /// Client ID used during authorization.
        client_id: String,
        /// Client secret (for confidential clients).
        client_secret: Option<String>,
    },
}

impl AuthState {
    /// Whether this OAuth token is expired (within refresh window).
    pub fn is_expired(&self) -> bool {
        match self {
            AuthState::OAuth { expires_at, .. } => {
                use std::time::{SystemTime, UNIX_EPOCH};
                const REFRESH_SKEW: u64 = 60;
                match expires_at {
                    None => false,
                    Some(exp) => {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        now + REFRESH_SKEW >= *exp
                    }
                }
            }
            _ => false,
        }
    }
}

/// Context for proactive token refresh inside `HttpTransport`.
pub(crate) struct RefreshContext {
    server_name: String,
    server_url: String,
    store: Arc<CredentialsStore>,
}

impl HttpTransport {
    pub fn new(
        url: impl Into<String>,
        headers: &HashMap<String, String>,
        timeout_secs: u64,
        auth: Option<AuthState>,
    ) -> Result<Self> {
        Self::with_refresh(url, headers, timeout_secs, auth, None)
    }

    pub(crate) fn with_refresh(
        url: impl Into<String>,
        headers: &HashMap<String, String>,
        timeout_secs: u64,
        auth: Option<AuthState>,
        refresh_ctx: Option<RefreshContext>,
    ) -> Result<Self> {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_static("2024-11-05"),
        );
        default_headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        // Per MCP Streamable HTTP spec: clients SHOULD accept both JSON and SSE.
        default_headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        for (k, v) in headers {
            let name = HeaderName::from_bytes(k.as_bytes())
                .with_context(|| format!("invalid header name: {k}"))?;
            let value = HeaderValue::from_str(v)
                .with_context(|| format!("invalid header value for {k}"))?;
            default_headers.insert(name, value);
        }

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .build()?;

        let timeout = if timeout_secs == 0 {
            DEFAULT_REQUEST_TIMEOUT
        } else {
            Duration::from_secs(timeout_secs)
        };

        Ok(Self {
            client,
            url: url.into(),
            next_id: AtomicU64::new(1),
            timeout,
            auth: Arc::new(Mutex::new(auth)),
            refresh_ctx,
            session_id: Arc::new(Mutex::new(None)),
        })
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub async fn set_auth(&self, auth: AuthState) {
        *self.auth.lock().await = Some(auth);
    }

    pub async fn auth(&self) -> Option<AuthState> {
        self.auth.lock().await.clone()
    }

    pub async fn send_notification(&self, notif: &JsonRpcNotification) -> Result<()> {
        let body = serde_json::to_string(notif)?;
        let req = self.build_request(body).await?;
        // Send notification and capture any session ID from the response header,
        // but do not require a meaningful JSON body back (202 Accepted is typical).
        if let Ok(Ok(resp)) = tokio::time::timeout(self.timeout, req.send()).await {
            self.capture_session_id(&resp).await;
        }
        Ok(())
    }

    pub async fn send_request(&self, req: &JsonRpcRequest) -> Result<Value> {
        let body = serde_json::to_string(req)?;

        trace!(method = %req.method, id = req.id, "MCP → HTTP server");

        let http_req = self.build_request(body).await?;
        let resp = tokio::time::timeout(self.timeout, http_req.send())
            .await
            .map_err(|_| anyhow!("MCP HTTP request timed out: {}", req.method))?
            .with_context(|| format!("MCP HTTP request failed: {}", req.method))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let www_authenticate = resp
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            return Err(anyhow::Error::new(UnauthorizedError {
                url: self.url.clone(),
                www_authenticate,
            }));
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "MCP server returned HTTP {}: {}",
                status,
                text.chars().take(500).collect::<String>()
            ));
        }

        // Capture session ID before consuming the response body.
        self.capture_session_id(&resp).await;

        // The Streamable HTTP spec allows the server to respond with either:
        // - application/json  → direct JSON-RPC response
        // - text/event-stream → SSE stream carrying one or more JSON-RPC messages
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        if content_type.contains("text/event-stream") {
            return self.parse_sse_response(resp, req.id).await;
        }

        let rpc_resp: JsonRpcResponse = resp
            .json()
            .await
            .context("parse MCP HTTP response as JSON-RPC")?;

        if let Some(err) = rpc_resp.error {
            return Err(anyhow!("{}", err));
        }

        rpc_resp
            .result
            .ok_or_else(|| anyhow!("MCP HTTP response missing result"))
    }

    /// Parse a `text/event-stream` response and return the JSON-RPC result
    /// for the given request ID.
    ///
    /// Each SSE event has the form `data: <json>\n\n`.  We scan for the first
    /// event whose JSON payload matches the expected `id` and return its result.
    async fn parse_sse_response(&self, resp: reqwest::Response, expected_id: u64) -> Result<Value> {
        let text = resp.text().await.context("read SSE response body")?;

        trace!(body_len = text.len(), "MCP ← SSE response");

        // Events are separated by blank lines; each field is "key: value".
        for event_block in text.split("\n\n") {
            let data = event_block
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("");

            if data.is_empty() {
                continue;
            }

            let rpc_resp: JsonRpcResponse = match serde_json::from_str(&data) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let matches = match &rpc_resp.id {
                Some(Value::Number(n)) => n.as_u64() == Some(expected_id),
                Some(Value::String(s)) => s.parse::<u64>().ok() == Some(expected_id),
                _ => false,
            };

            if !matches {
                continue;
            }

            if let Some(err) = rpc_resp.error {
                return Err(anyhow!("{}", err));
            }

            return rpc_resp
                .result
                .ok_or_else(|| anyhow!("MCP SSE response missing result"));
        }

        Err(anyhow!(
            "no matching JSON-RPC response found in SSE stream for request id {}",
            expected_id
        ))
    }

    async fn build_request(&self, body: String) -> Result<reqwest::RequestBuilder> {
        // Proactively refresh OAuth tokens if near expiry.
        self.maybe_refresh_token().await;

        let mut rb = self.client.post(&self.url).body(body);

        // Include session ID if the server provided one during initialize.
        if let Some(ref sid) = *self.session_id.lock().await {
            rb = rb.header("mcp-session-id", sid.as_str());
        }

        if let Some(auth) = &*self.auth.lock().await {
            match auth {
                AuthState::BearerToken(token) => {
                    rb = rb.bearer_auth(token);
                }
                AuthState::OAuth { access_token, .. } => {
                    rb = rb.bearer_auth(access_token);
                }
            }
        }
        Ok(rb)
    }

    /// Capture `mcp-session-id` from a response header.
    ///
    /// Stateful servers return this on the `initialize` response; we store it
    /// and re-send it on every subsequent request in the same session.
    async fn capture_session_id(&self, resp: &reqwest::Response) {
        // Try both `mcp-session-id` and `Mcp-Session-Id` (case-insensitive via reqwest).
        let sid = resp
            .headers()
            .get("mcp-session-id")
            .or_else(|| resp.headers().get("Mcp-Session-Id"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        if let Some(ref s) = sid {
            debug!(session_id = %s, "MCP session ID captured");
            *self.session_id.lock().await = sid;
        }
    }

    async fn maybe_refresh_token(&self) {
        let needs_refresh = {
            let auth_guard = self.auth.lock().await;
            auth_guard.as_ref().map_or(false, |a| a.is_expired())
        };

        if !needs_refresh {
            return;
        }

        let ctx = match &self.refresh_ctx {
            Some(c) => c,
            None => return,
        };

        let stored_opt = {
            let auth_guard = self.auth.lock().await;
            match auth_guard.as_ref() {
                Some(AuthState::OAuth {
                    access_token,
                    refresh_token: Some(rt),
                    expires_at,
                    token_endpoint,
                    client_id,
                    client_secret,
                }) => Some(StoredTokens {
                    server_name: ctx.server_name.clone(),
                    server_url: ctx.server_url.clone(),
                    access_token: access_token.clone(),
                    refresh_token: Some(rt.clone()),
                    expires_at: *expires_at,
                    token_endpoint: token_endpoint.clone(),
                    client_id: Some(client_id.clone()),
                    client_secret: client_secret.clone(),
                }),
                _ => None,
            }
        };

        let stored = match stored_opt {
            Some(s) => s,
            None => return,
        };

        debug!(server = %ctx.server_name, "proactively refreshing OAuth token");

        match refresh_token(&self.client, &stored).await {
            Ok(fresh) => {
                if let Err(e) = ctx.store.save(&fresh) {
                    warn!(error = %e, "failed to persist proactively refreshed token");
                }
                let new_auth = AuthState::OAuth {
                    access_token: fresh.access_token,
                    refresh_token: fresh.refresh_token,
                    expires_at: fresh.expires_at,
                    token_endpoint: fresh.token_endpoint,
                    client_id: fresh
                        .client_id
                        .unwrap_or_else(|| "sven-mcp-client".to_string()),
                    client_secret: fresh.client_secret,
                };
                *self.auth.lock().await = Some(new_auth);
                debug!(server = %ctx.server_name, "OAuth token refreshed proactively");
            }
            Err(e) => {
                warn!(
                    server = %ctx.server_name,
                    error = %e,
                    "proactive token refresh failed"
                );
            }
        }
    }
}

// ── Transport enum ────────────────────────────────────────────────────────────

pub enum Transport {
    Stdio(Box<StdioTransport>),
    Http(HttpTransport),
}

impl Transport {
    pub fn next_id(&self) -> u64 {
        match self {
            Transport::Stdio(t) => t.next_id(),
            Transport::Http(t) => t.next_id(),
        }
    }

    pub async fn send_notification(&self, notif: &JsonRpcNotification) -> Result<()> {
        match self {
            Transport::Stdio(t) => t.send_notification(notif).await,
            Transport::Http(t) => t.send_notification(notif).await,
        }
    }

    pub async fn send_request(&self, req: &JsonRpcRequest) -> Result<Value> {
        match self {
            Transport::Stdio(t) => t.send_request(req).await,
            Transport::Http(t) => t.send_request(req).await,
        }
    }

    pub fn is_http(&self) -> bool {
        matches!(self, Transport::Http(_))
    }

    pub fn as_http(&self) -> Option<&HttpTransport> {
        match self {
            Transport::Http(t) => Some(t),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Stdio(_) => write!(f, "Transport::Stdio"),
            Transport::Http(_) => write!(f, "Transport::Http"),
        }
    }
}

// ── Public constructor helpers ────────────────────────────────────────────────

pub fn build_http_transport(
    url: &str,
    headers: &HashMap<String, String>,
    timeout_secs: u64,
    auth: Option<AuthState>,
    server_name: &str,
    server_url: &str,
    store: Arc<CredentialsStore>,
) -> Result<HttpTransport> {
    let refresh_ctx = if matches!(auth, Some(AuthState::OAuth { .. })) {
        Some(RefreshContext {
            server_name: server_name.to_string(),
            server_url: server_url.to_string(),
            store,
        })
    } else {
        None
    };

    HttpTransport::with_refresh(url, headers, timeout_secs, auth, refresh_ctx)
}
