// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! MCP transport implementations (stdio subprocess and HTTP POST).
//!
//! Both transports implement a single `send_request` method that sends a
//! JSON-RPC 2.0 request and waits for the response.  Notifications
//! (initialize confirmation) are fire-and-forget via `send_notification`.

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
///
/// Spawns a child process and communicates with it using newline-delimited
/// JSON-RPC 2.0 over stdin/stdout.
pub struct StdioTransport {
    /// Child process (kept alive for the lifetime of the transport).
    _child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl StdioTransport {
    /// Spawn `command args` with the given environment variables and create
    /// the transport.
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

        // Pass through a minimal safe environment + user-specified vars.
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

    /// Send a JSON-RPC notification (no response expected).
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

    /// Send a JSON-RPC request and wait for the matching response.
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

        // Read lines until we find one whose id matches our request.
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
                    // Server may emit notifications; skip them.
                    debug!(line = %trimmed, error = %e, "skipping non-response line");
                    continue;
                }
            };

            // Match on id.
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
///
/// Each JSON-RPC request is sent as a `POST /mcp` with `application/json`
/// body.  The server responds with the JSON-RPC response in the body.
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    next_id: AtomicU64,
    timeout: Duration,
    auth: Arc<Mutex<Option<AuthState>>>,
}

/// Authentication state for an HTTP MCP server.
#[derive(Debug, Clone)]
pub enum AuthState {
    /// A static bearer token (from config headers).
    BearerToken(String),
    /// OAuth access token.
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<u64>,
    },
}

impl HttpTransport {
    pub fn new(
        url: impl Into<String>,
        headers: &HashMap<String, String>,
        timeout_secs: u64,
        auth: Option<AuthState>,
    ) -> Result<Self> {
        let mut default_headers = HeaderMap::new();
        // MCP protocol version header
        default_headers.insert(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_static("2024-11-05"),
        );
        default_headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
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
        })
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Update the authentication state (e.g., after OAuth token exchange).
    pub async fn set_auth(&self, auth: AuthState) {
        *self.auth.lock().await = Some(auth);
    }

    /// Get the current auth state.
    pub async fn auth(&self) -> Option<AuthState> {
        self.auth.lock().await.clone()
    }

    /// Send a JSON-RPC notification (fire and forget for HTTP).
    pub async fn send_notification(&self, notif: &JsonRpcNotification) -> Result<()> {
        let body = serde_json::to_string(notif)?;
        let req = self.build_request(body).await?;
        // For notifications we ignore the response body but still send the request.
        let _ = tokio::time::timeout(self.timeout, req.send()).await;
        Ok(())
    }

    /// Send a JSON-RPC request and return the result.
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
                "MCP HTTP error {}: {}",
                status,
                text.chars().take(200).collect::<String>()
            ));
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

    async fn build_request(&self, body: String) -> Result<reqwest::RequestBuilder> {
        let mut rb = self.client.post(&self.url).body(body);
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
}

// ── Transport enum (avoids boxing dyn) ───────────────────────────────────────

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

// ── Logging helpers ───────────────────────────────────────────────────────────

/// Trim long error messages from noisy server processes.
pub fn trim_server_error(err: &str) -> String {
    let trimmed = err.trim();
    if trimmed.len() > 400 {
        format!("{}…", &trimmed[..400])
    } else {
        trimmed.to_string()
    }
}

/// Emit a warning for server stderr output exceeding a threshold.
pub fn maybe_warn_stderr(name: &str, msg: &str) {
    if !msg.trim().is_empty() {
        warn!(server = %name, "MCP server stderr: {}", trim_server_error(msg));
    }
}
