// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Multi-server MCP connection manager.
//!
//! `McpManager` owns all live server connections, handles connect/disconnect
//! lifecycle, exposes a unified tool and prompt catalogue, and drives the
//! reconnection loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use sven_config::{McpServerConfig, McpTransport};

use sven_tools::Tool as _;

use crate::bridge::{McpPromptArgInfo, McpPromptInfo, McpTool};
use crate::client::McpConnection;
use crate::health::{HealthState, ServerStatus, ServerStatusSummary};
use crate::oauth::{
    discover_oauth_info, ensure_fresh, run_oauth_flow, CredentialsStore, OAuthContext,
};
use crate::transport::{
    build_http_transport, AuthState, StdioTransport, Transport, UnauthorizedError,
};

// ── McpEvent ──────────────────────────────────────────────────────────────────

/// Events emitted by the manager to signal state changes.
#[derive(Debug)]
pub enum McpEvent {
    /// A server successfully connected.
    ServerConnected(String),
    /// A server disconnected or was disabled.
    ServerDisconnected(String),
    /// A server connection failed.
    ServerFailed { name: String, error: String },
    /// The set of tools/prompts changed; re-register with the tool registry.
    ToolsChanged,
    /// An HTTP server requires OAuth authentication.
    AuthRequired { server: String, auth_url: String },
    /// OAuth authentication started (browser opened).
    AuthStarted { server: String },
}

// ── ServerState ───────────────────────────────────────────────────────────────

struct ServerState {
    connection: Option<McpConnection>,
    health: HealthState,
    tools: Vec<crate::protocol::McpTool>,
    prompts: Vec<crate::protocol::McpPrompt>,
    /// Set while an OAuth flow is in progress to prevent spawning a second
    /// concurrent flow if another 401 arrives before the first completes.
    auth_in_progress: bool,
}

impl ServerState {
    fn new_disabled() -> Self {
        let mut health = HealthState::new();
        health.status = ServerStatus::Disabled;
        Self {
            connection: None,
            health,
            tools: vec![],
            prompts: vec![],
            auth_in_progress: false,
        }
    }

    fn new() -> Self {
        Self {
            connection: None,
            health: HealthState::new(),
            tools: vec![],
            prompts: vec![],
            auth_in_progress: false,
        }
    }
}

// ── McpManager ────────────────────────────────────────────────────────────────

/// Multi-server MCP connection manager.
pub struct McpManager {
    config: RwLock<HashMap<String, McpServerConfig>>,
    servers: RwLock<HashMap<String, ServerState>>,
    store: Arc<CredentialsStore>,
    http_client: reqwest::Client,
    event_tx: mpsc::Sender<McpEvent>,
    /// When false (headless/CI mode), never trigger interactive OAuth flows.
    /// Batch runs must complete without user interaction.
    allow_interactive_oauth: bool,
}

impl McpManager {
    /// Create a new manager with the given initial config.
    ///
    /// Set `allow_interactive_oauth` to `false` for headless, batch, or CI runs
    /// so that OAuth flows (browser open, user login) are never triggered.
    pub fn new(
        config: HashMap<String, McpServerConfig>,
        event_tx: mpsc::Sender<McpEvent>,
        allow_interactive_oauth: bool,
    ) -> Arc<Self> {
        let http_client = reqwest::Client::builder().build().unwrap_or_default();

        Arc::new(Self {
            config: RwLock::new(config),
            servers: RwLock::new(HashMap::new()),
            store: Arc::new(CredentialsStore::with_default_path()),
            http_client,
            event_tx,
            allow_interactive_oauth,
        })
    }

    // ── Background tasks ──────────────────────────────────────────────────────

    /// Start background tasks: reconnection loop.
    ///
    /// Call once after `connect_all()`.  The loop periodically checks for
    /// servers in the `Reconnecting` state and retries them when the backoff
    /// delay has elapsed.
    pub fn start_background_tasks(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;

                let servers_to_retry: Vec<String> = {
                    let servers = this.servers.read().await;
                    servers
                        .iter()
                        .filter(|(_, state)| state.health.should_retry_now())
                        .map(|(name, _)| name.clone())
                        .collect()
                };

                for name in servers_to_retry {
                    let this2 = Arc::clone(&this);
                    let n = name.clone();
                    tokio::spawn(async move {
                        debug!(server = %n, "background reconnect attempt");
                        if let Err(e) = this2.connect(&n).await {
                            debug!(server = %n, error = %e, "background reconnect failed");
                        }
                    });
                }
            }
        });
    }

    // ── Connection management ─────────────────────────────────────────────────

    /// Connect to all enabled servers in the config.
    pub async fn connect_all(self: &Arc<Self>) {
        let names: Vec<String> = self
            .config
            .read()
            .await
            .iter()
            .filter(|(_, c)| c.enabled)
            .map(|(n, _)| n.clone())
            .collect();

        for name in names {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(e) = this.connect(&name).await {
                    warn!(server = %name, error = %e, "MCP server connect failed");
                }
            });
        }
    }

    /// Connect to a single named server (by name from config).
    pub async fn connect(self: &Arc<Self>, name: &str) -> Result<()> {
        let cfg = {
            let guard = self.config.read().await;
            guard
                .get(name)
                .cloned()
                .with_context(|| format!("no MCP server config for {name}"))?
        };

        if !cfg.enabled {
            let mut servers = self.servers.write().await;
            servers
                .entry(name.to_string())
                .or_insert_with(ServerState::new_disabled);
            return Ok(());
        }

        {
            let mut servers = self.servers.write().await;
            let state = servers
                .entry(name.to_string())
                .or_insert_with(ServerState::new);
            state.health.status = ServerStatus::Connecting;
        }

        match self.try_connect(name, &cfg).await {
            Ok((conn, tools, prompts)) => {
                let tc = tools.len();
                let pc = prompts.len();
                {
                    let mut servers = self.servers.write().await;
                    let state = servers
                        .entry(name.to_string())
                        .or_insert_with(ServerState::new);
                    state.connection = Some(conn);
                    state.tools = tools;
                    state.prompts = prompts;
                    state.health.report_ok(tc, pc);
                }
                info!(server = %name, tools = tc, prompts = pc, "MCP server connected");
                let _ = self
                    .event_tx
                    .send(McpEvent::ServerConnected(name.to_string()))
                    .await;
                let _ = self.event_tx.send(McpEvent::ToolsChanged).await;
                Ok(())
            }
            Err(e) => {
                if let Some(unauth) = e.downcast_ref::<UnauthorizedError>() {
                    self.handle_unauthorized(name, &cfg, unauth);
                } else {
                    // Use the full anyhow chain so the error message includes
                    // the underlying HTTP response body or transport error.
                    let error_str = format!("{:#}", e);
                    let mut servers = self.servers.write().await;
                    let state = servers
                        .entry(name.to_string())
                        .or_insert_with(ServerState::new);
                    state.health.report_error(error_str.clone());
                    let _ = self
                        .event_tx
                        .send(McpEvent::ServerFailed {
                            name: name.to_string(),
                            error: error_str,
                        })
                        .await;
                }
                Err(e)
            }
        }
    }

    /// Handle a 401 Unauthorized response from an MCP server.
    ///
    /// Sets the status to `NeedsAuth` and emits an event. If the server config
    /// has an `oauth` section, also automatically spawns the browser-based
    /// OAuth flow (background task).
    fn handle_unauthorized(
        self: &Arc<Self>,
        name: &str,
        cfg: &McpServerConfig,
        unauth: &UnauthorizedError,
    ) {
        let www_auth = unauth.www_authenticate.clone();
        let server_url = match &cfg.transport {
            McpTransport::Http { url, .. } => url.clone(),
            _ => return,
        };

        let config_scopes = cfg
            .oauth
            .as_ref()
            .map(|o| o.scopes.as_slice())
            .unwrap_or(&[])
            .to_vec();
        let has_oauth_config = cfg.oauth.is_some();

        let this = Arc::clone(self);
        let name_owned = name.to_string();

        tokio::spawn(async move {
            // Check and set the auth_in_progress flag atomically.
            // If an auth flow is already running for this server, skip starting
            // another one (prevents infinite re-auth loops on persistent 401s).
            let should_start_auth = {
                let mut servers = this.servers.write().await;
                let state = servers
                    .entry(name_owned.clone())
                    .or_insert_with(ServerState::new);
                if state.auth_in_progress {
                    debug!(server = %name_owned, "auth already in progress, skipping duplicate");
                    false
                } else {
                    state.auth_in_progress = has_oauth_config;
                    state.health.status = ServerStatus::NeedsAuth {
                        auth_url: String::new(),
                    };
                    true
                }
            };

            if !should_start_auth {
                return;
            }

            // Discover the authorization server URL for display purposes.
            let discovery_result = discover_oauth_info(
                &this.http_client,
                &server_url,
                www_auth.as_deref(),
                &config_scopes,
            )
            .await;

            let auth_url = match discovery_result {
                Ok(ref disc) => {
                    let client_id = {
                        let cfg_guard = this.config.read().await;
                        cfg_guard
                            .get(&name_owned)
                            .and_then(|c| c.oauth.as_ref())
                            .and_then(|o| o.client_id.clone())
                    };
                    let code_verifier = crate::oauth::generate_code_verifier();
                    let state_token = crate::oauth::generate_state();
                    let ctx = OAuthContext {
                        code_verifier,
                        state: state_token,
                        server_url: server_url.clone(),
                        metadata: disc.auth_server_metadata.clone(),
                        redirect_uri: OAuthContext::callback_uri(19876),
                        client_id,
                    };
                    ctx.authorization_url(&disc.scopes).unwrap_or_default()
                }
                Err(ref e) => {
                    warn!(server = %name_owned, error = %e, "OAuth discovery failed");
                    String::new()
                }
            };

            // Update status with the discovered auth URL.
            {
                let mut servers = this.servers.write().await;
                if let Some(state) = servers.get_mut(&name_owned) {
                    state.health.status = ServerStatus::NeedsAuth {
                        auth_url: auth_url.clone(),
                    };
                }
            }

            if has_oauth_config && this.allow_interactive_oauth {
                let _ = this
                    .event_tx
                    .send(McpEvent::AuthStarted {
                        server: name_owned.clone(),
                    })
                    .await;

                let auth_result = this
                    .authenticate_with_www_auth(&name_owned, www_auth.as_deref())
                    .await;

                // Clear the in-progress flag regardless of outcome.
                {
                    let mut servers = this.servers.write().await;
                    if let Some(state) = servers.get_mut(&name_owned) {
                        state.auth_in_progress = false;
                    }
                }

                match auth_result {
                    Ok(_) => {
                        debug!(server = %name_owned, "auto-auth completed successfully");
                    }
                    Err(e) => {
                        warn!(server = %name_owned, error = %e, "auto-auth failed");
                        let error_str = format!("{:#}", e);
                        let mut servers = this.servers.write().await;
                        if let Some(state) = servers.get_mut(&name_owned) {
                            state.health.report_error(error_str.clone());
                        }
                        let _ = this
                            .event_tx
                            .send(McpEvent::ServerFailed {
                                name: name_owned,
                                error: error_str,
                            })
                            .await;
                    }
                }
            } else if has_oauth_config && !this.allow_interactive_oauth {
                // Headless/CI mode: never trigger interactive OAuth. Emit AuthRequired
                // so the run can fail fast; batch flows must not block on user input.
                {
                    let mut servers = this.servers.write().await;
                    if let Some(state) = servers.get_mut(&name_owned) {
                        state.auth_in_progress = false;
                    }
                }
                let _ = this
                    .event_tx
                    .send(McpEvent::AuthRequired {
                        server: name_owned,
                        auth_url,
                    })
                    .await;
            } else {
                // Manual auth required — user must run `/mcp auth <name>`.
                let _ = this
                    .event_tx
                    .send(McpEvent::AuthRequired {
                        server: name_owned,
                        auth_url,
                    })
                    .await;
            }
        });
    }

    /// Disconnect and remove a server.
    pub async fn disconnect(&self, name: &str) {
        let mut servers = self.servers.write().await;
        if let Some(state) = servers.get_mut(name) {
            state.connection = None;
            state.tools.clear();
            state.prompts.clear();
            state.health.status = ServerStatus::Disabled;
        }
        let _ = self
            .event_tx
            .send(McpEvent::ServerDisconnected(name.to_string()))
            .await;
        let _ = self.event_tx.send(McpEvent::ToolsChanged).await;
    }

    // ── Tool access ───────────────────────────────────────────────────────────

    pub async fn tools(self: &Arc<Self>) -> Vec<McpTool> {
        let servers = self.servers.read().await;
        let mut tools = Vec::new();
        for (name, state) in servers.iter() {
            if !state.health.status.is_connected() {
                continue;
            }
            for t in &state.tools {
                tools.push(McpTool::new(
                    name.clone(),
                    t.name.clone(),
                    t.description.clone(),
                    t.input_schema.clone(),
                    Arc::clone(self),
                ));
            }
        }
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
    }

    pub async fn prompts(&self) -> Vec<McpPromptInfo> {
        let servers = self.servers.read().await;
        let mut prompts = Vec::new();
        for (name, state) in servers.iter() {
            if !state.health.status.is_connected() {
                continue;
            }
            for p in &state.prompts {
                let args = p
                    .arguments
                    .as_ref()
                    .map(|args| {
                        args.iter()
                            .map(|a| McpPromptArgInfo {
                                name: a.name.clone(),
                                description: a.description.clone().unwrap_or_default(),
                                required: a.required.unwrap_or(false),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                prompts.push(McpPromptInfo {
                    slash_path: format!("{}/{}", name, p.name),
                    server_name: name.clone(),
                    prompt_name: p.name.clone(),
                    description: p.description.clone().unwrap_or_default(),
                    arguments: args,
                });
            }
        }
        prompts.sort_by(|a, b| a.slash_path.cmp(&b.slash_path));
        prompts
    }

    // ── Tool execution ────────────────────────────────────────────────────────

    pub async fn call_tool(&self, server: &str, tool: &str, args: &Value) -> Result<String> {
        let servers = self.servers.read().await;
        let state = servers
            .get(server)
            .with_context(|| format!("no MCP server state for {server}"))?;
        let conn = state
            .connection
            .as_ref()
            .with_context(|| format!("MCP server {server} is not connected"))?;
        conn.call_tool(tool, args).await
    }

    pub async fn get_prompt(
        &self,
        server: &str,
        prompt: &str,
        args: HashMap<String, String>,
    ) -> Result<String> {
        let servers = self.servers.read().await;
        let state = servers
            .get(server)
            .with_context(|| format!("no MCP server state for {server}"))?;
        let conn = state
            .connection
            .as_ref()
            .with_context(|| format!("MCP server {server} is not connected"))?;
        conn.get_prompt(prompt, &args).await
    }

    // ── Server status ─────────────────────────────────────────────────────────

    pub async fn server_statuses(&self) -> Vec<ServerStatusSummary> {
        let servers = self.servers.read().await;
        let config = self.config.read().await;

        let mut names: Vec<String> = config.keys().cloned().collect();
        names.sort();

        names
            .into_iter()
            .map(|name| {
                let (status, tool_count, prompt_count) = servers
                    .get(&name)
                    .map(|s| {
                        (
                            s.health.status.clone(),
                            s.health.tool_count,
                            s.health.prompt_count,
                        )
                    })
                    .unwrap_or((ServerStatus::Initializing, 0, 0));
                ServerStatusSummary {
                    name,
                    status,
                    tool_count,
                    prompt_count,
                }
            })
            .collect()
    }

    // ── Config management ─────────────────────────────────────────────────────

    pub async fn update_config(self: &Arc<Self>, new_config: HashMap<String, McpServerConfig>) {
        let (to_remove, to_add, to_update) = {
            let current = self.config.read().await;
            let mut to_remove = Vec::new();
            let mut to_add = Vec::new();
            let mut to_update = Vec::new();

            for name in current.keys() {
                if !new_config.contains_key(name) {
                    to_remove.push(name.clone());
                }
            }

            for (name, new_cfg) in &new_config {
                match current.get(name) {
                    None => to_add.push(name.clone()),
                    Some(old_cfg) => {
                        if config_changed(old_cfg, new_cfg) {
                            to_update.push(name.clone());
                        }
                    }
                }
            }

            (to_remove, to_add, to_update)
        };

        {
            let mut config = self.config.write().await;
            *config = new_config;
        }

        for name in to_remove {
            self.disconnect(&name).await;
        }

        for name in to_update {
            self.disconnect(&name).await;
            let this = Arc::clone(self);
            let n = name.clone();
            tokio::spawn(async move {
                if let Err(e) = this.connect(&n).await {
                    warn!(server = %n, error = %e, "MCP server reconnect failed");
                }
            });
        }

        for name in to_add {
            let this = Arc::clone(self);
            let n = name.clone();
            tokio::spawn(async move {
                if let Err(e) = this.connect(&n).await {
                    warn!(server = %n, error = %e, "MCP server initial connect failed");
                }
            });
        }
    }

    // ── OAuth support ─────────────────────────────────────────────────────────

    /// Run the full interactive OAuth PKCE flow (opens browser, waits for
    /// callback) for `server`.
    ///
    /// This method blocks until the user completes browser-based authentication
    /// (up to 5 minutes) or an error occurs.  On success, tokens are persisted
    /// and the server is automatically reconnected.
    pub async fn authenticate(self: &Arc<Self>, server: &str) -> Result<String> {
        self.authenticate_with_www_auth(server, None).await
    }

    /// Like [`authenticate`] but accepts `www_authenticate` from a 401 response.
    /// Use this when re-auth is triggered by an HTTP 401 so discovery can use
    /// the server's `resource_metadata` URL if present.
    pub async fn authenticate_with_www_auth(
        self: &Arc<Self>,
        server: &str,
        www_authenticate: Option<&str>,
    ) -> Result<String> {
        if !self.allow_interactive_oauth {
            anyhow::bail!(
                "OAuth flow cannot run in headless/CI mode. \
                 Pre-authenticate in an interactive session before running batch."
            );
        }
        let cfg = self.server_config(server).await?;

        let url = match &cfg.transport {
            McpTransport::Http { url, .. } => url.clone(),
            _ => {
                return Err(anyhow::anyhow!(
                    "OAuth is only supported for HTTP MCP servers"
                ))
            }
        };

        let config_scopes = cfg
            .oauth
            .as_ref()
            .map(|o| o.scopes.as_slice())
            .unwrap_or(&[]);
        let client_id = cfg.oauth.as_ref().and_then(|o| o.client_id.clone());
        let client_secret = cfg.oauth.as_ref().and_then(|o| o.client_secret.clone());

        let discovery =
            discover_oauth_info(&self.http_client, &url, www_authenticate, config_scopes).await?;

        let tokens = run_oauth_flow(
            &self.http_client,
            server,
            &url,
            discovery,
            client_id,
            client_secret,
            &self.store,
        )
        .await?;

        // Reconnect with fresh tokens.
        let this = Arc::clone(self);
        let name = server.to_string();
        tokio::spawn(async move {
            if let Err(e) = this.connect(&name).await {
                warn!(server = %name, error = %e, "reconnect after OAuth failed");
            }
        });

        Ok(format!(
            "Authenticated as {}",
            tokens.access_token.chars().take(8).collect::<String>()
        ))
    }

    /// Look up the config for a named server.
    async fn server_config(&self, server: &str) -> Result<McpServerConfig> {
        self.config
            .read()
            .await
            .get(server)
            .cloned()
            .with_context(|| format!("no config for server {server}"))
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn try_connect(
        &self,
        name: &str,
        cfg: &McpServerConfig,
    ) -> Result<(
        McpConnection,
        Vec<crate::protocol::McpTool>,
        Vec<crate::protocol::McpPrompt>,
    )> {
        let transport = self.build_transport(name, cfg).await?;
        let conn = McpConnection::initialize(transport, name).await?;

        let tools = conn.list_tools().await.unwrap_or_else(|e| {
            warn!(server = %name, error = %e, "tools/list failed");
            vec![]
        });

        let prompts = conn.list_prompts().await.unwrap_or_else(|e| {
            debug!(server = %name, error = %e, "prompts/list failed (server may not support it)");
            vec![]
        });

        Ok((conn, tools, prompts))
    }

    async fn build_transport(&self, name: &str, cfg: &McpServerConfig) -> Result<Transport> {
        match &cfg.transport {
            McpTransport::Stdio { command, args } => {
                let t = StdioTransport::spawn(command, args, &cfg.env, cfg.timeout_secs).await?;
                Ok(Transport::Stdio(Box::new(t)))
            }
            McpTransport::Http { url, headers } => {
                let auth = self.load_http_auth(name, url, cfg).await;
                let t = build_http_transport(
                    url,
                    headers,
                    cfg.timeout_secs,
                    auth,
                    name,
                    url,
                    Arc::clone(&self.store),
                )?;
                Ok(Transport::Http(t))
            }
        }
    }

    async fn load_http_auth(
        &self,
        name: &str,
        url: &str,
        cfg: &McpServerConfig,
    ) -> Option<AuthState> {
        // 1. Check for stored OAuth tokens, refresh if near expiry.
        if let Some(stored) = self.store.load(name, url) {
            match ensure_fresh(&self.http_client, stored, &self.store).await {
                Ok(fresh) => {
                    return Some(AuthState::OAuth {
                        access_token: fresh.access_token,
                        refresh_token: fresh.refresh_token,
                        expires_at: fresh.expires_at,
                        token_endpoint: fresh.token_endpoint,
                        client_id: fresh
                            .client_id
                            .unwrap_or_else(|| "sven-mcp-client".to_string()),
                        client_secret: fresh.client_secret,
                    });
                }
                Err(e) => {
                    // Refresh failed or token completely expired — clear and let
                    // the connection attempt trigger fresh OAuth.
                    warn!(server = %name, error = %e, "stored token invalid, will re-authenticate");
                    return None;
                }
            }
        }

        // 2. Fall back to a bearer token in the configured headers.
        if let McpTransport::Http { headers, .. } = &cfg.transport {
            if let Some(auth_header) = headers.get("Authorization") {
                if let Some(token) = auth_header.strip_prefix("Bearer ") {
                    return Some(AuthState::BearerToken(token.to_string()));
                }
            }
        }

        None
    }
}

// ── Config change detection ───────────────────────────────────────────────────

fn config_changed(old: &McpServerConfig, new: &McpServerConfig) -> bool {
    if old.enabled != new.enabled {
        return true;
    }
    if old.timeout_secs != new.timeout_secs {
        return true;
    }
    if old.env != new.env {
        return true;
    }
    let old_t = serde_json::to_string(&old.transport).unwrap_or_default();
    let new_t = serde_json::to_string(&new.transport).unwrap_or_default();
    old_t != new_t
}
