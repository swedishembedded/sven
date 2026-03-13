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

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use sven_config::{McpServerConfig, McpTransport};

use sven_tools::Tool as _;

use crate::bridge::{McpPromptArgInfo, McpPromptInfo, McpTool};
use crate::client::McpConnection;
use crate::health::{HealthState, ServerStatus, ServerStatusSummary};
use crate::oauth::{ensure_fresh, run_oauth_flow, CredentialsStore};
use crate::transport::{AuthState, HttpTransport, StdioTransport, Transport};

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
}

// ── ServerState ───────────────────────────────────────────────────────────────

/// Per-server runtime state.
struct ServerState {
    connection: Option<McpConnection>,
    health: HealthState,
    tools: Vec<crate::protocol::McpTool>,
    prompts: Vec<crate::protocol::McpPrompt>,
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
        }
    }

    fn new() -> Self {
        Self {
            connection: None,
            health: HealthState::new(),
            tools: vec![],
            prompts: vec![],
        }
    }
}

// ── McpManager ────────────────────────────────────────────────────────────────

/// Multi-server MCP connection manager.
///
/// All access is guarded by a single `RwLock`.  The manager is shared via
/// `Arc<McpManager>` between the agent loop and TUI.
pub struct McpManager {
    /// Current config per server name.
    config: RwLock<HashMap<String, McpServerConfig>>,
    /// Live connection + health state per server name.
    servers: RwLock<HashMap<String, ServerState>>,
    /// Token store for OAuth credentials.
    store: CredentialsStore,
    /// HTTP client shared across all HTTP transport connections.
    http_client: reqwest::Client,
    /// Channel for notifying the consumer about lifecycle events.
    event_tx: mpsc::Sender<McpEvent>,
}

impl McpManager {
    /// Create a new manager with the given initial config.
    ///
    /// Call `connect_all()` after construction to establish connections.
    pub fn new(
        config: HashMap<String, McpServerConfig>,
        event_tx: mpsc::Sender<McpEvent>,
    ) -> Arc<Self> {
        let http_client = reqwest::Client::builder().build().unwrap_or_default();

        Arc::new(Self {
            config: RwLock::new(config),
            servers: RwLock::new(HashMap::new()),
            store: CredentialsStore::with_default_path(),
            http_client,
            event_tx,
        })
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
                let error_str = e.to_string();
                // Check if this is an auth requirement.
                if error_str.contains("HTTP 401") || error_str.contains("authentication") {
                    let mut servers = self.servers.write().await;
                    let state = servers
                        .entry(name.to_string())
                        .or_insert_with(ServerState::new);
                    state.health.status = ServerStatus::NeedsAuth {
                        auth_url: String::new(),
                    };
                    let _ = self
                        .event_tx
                        .send(McpEvent::AuthRequired {
                            server: name.to_string(),
                            auth_url: String::new(),
                        })
                        .await;
                } else {
                    let mut servers = self.servers.write().await;
                    let state = servers
                        .entry(name.to_string())
                        .or_insert_with(ServerState::new);
                    state.health.report_error(error_str.clone());
                    let _ = self
                        .event_tx
                        .send(McpEvent::ServerFailed {
                            name: name.to_string(),
                            error: error_str.clone(),
                        })
                        .await;
                }
                Err(e)
            }
        }
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

    /// All tools from all connected servers, wrapped as `McpTool` instances.
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
        // Deterministic order: sorted by qualified name.
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
    }

    /// All prompts from all connected servers.
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

    /// Call a tool on the named server.
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

    /// Get a prompt from the named server.
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

    /// Summaries of all configured servers (connected or not).
    pub async fn server_statuses(&self) -> Vec<ServerStatusSummary> {
        let servers = self.servers.read().await;
        let config = self.config.read().await;

        // Include all configured servers, even those without a runtime state.
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

    /// Apply a new config, diffing against the current state.
    ///
    /// Only servers that changed (transport, env, oauth, or enabled flag) are
    /// reconnected.  Unchanged servers keep their live connections.
    pub async fn update_config(self: &Arc<Self>, new_config: HashMap<String, McpServerConfig>) {
        let (to_remove, to_add, to_update) = {
            let current = self.config.read().await;
            let mut to_remove = Vec::new();
            let mut to_add = Vec::new();
            let mut to_update = Vec::new();

            // Servers in old config but not in new.
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

        // Update the config.
        {
            let mut config = self.config.write().await;
            *config = new_config;
        }

        // Disconnect removed servers.
        for name in to_remove {
            self.disconnect(&name).await;
        }

        // Disconnect and reconnect updated servers.
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

        // Connect new servers.
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

    /// Start the OAuth flow for an HTTP server.
    ///
    /// Returns the authorization URL that the user should open in a browser.
    /// For TUI operation, this URL is displayed in a notification.
    pub async fn start_oauth(self: &Arc<Self>, server: &str) -> Result<String> {
        let cfg = {
            let guard = self.config.read().await;
            guard
                .get(server)
                .cloned()
                .with_context(|| format!("no config for server {server}"))?
        };

        let url = match &cfg.transport {
            McpTransport::Http { url, .. } => url.clone(),
            _ => {
                return Err(anyhow::anyhow!(
                    "OAuth is only supported for HTTP MCP servers"
                ))
            }
        };

        let oauth_cfg = cfg.oauth.as_ref();
        let scopes = oauth_cfg.map(|o| o.scopes.clone()).unwrap_or_default();
        let client_id = oauth_cfg.and_then(|o| o.client_id.clone());

        // Discover metadata and build the URL.
        let metadata = crate::oauth::discover_oauth_metadata(&self.http_client, &url).await?;

        let code_verifier = crate::oauth::generate_code_verifier();
        let state = crate::oauth::generate_state();
        let redirect_uri = crate::oauth::OAuthContext::callback_uri();

        let ctx = crate::oauth::OAuthContext {
            code_verifier,
            state: state.clone(),
            server_url: url.clone(),
            metadata,
            redirect_uri,
            client_id,
        };

        let auth_url = ctx.authorization_url(&scopes)?;

        // Store the pending auth context.
        {
            let mut servers = self.servers.write().await;
            let server_state = servers
                .entry(server.to_string())
                .or_insert_with(ServerState::new);
            server_state.health.status = ServerStatus::NeedsAuth {
                auth_url: auth_url.clone(),
            };
        }

        Ok(auth_url)
    }

    /// Complete OAuth by running the full interactive flow (opens browser, waits
    /// for callback).  Suitable for calling from a dedicated OAuth command.
    pub async fn authenticate(self: &Arc<Self>, server: &str) -> Result<String> {
        let cfg = {
            let guard = self.config.read().await;
            guard
                .get(server)
                .cloned()
                .with_context(|| format!("no config for server {server}"))?
        };

        let url = match &cfg.transport {
            McpTransport::Http { url, .. } => url.clone(),
            _ => {
                return Err(anyhow::anyhow!(
                    "OAuth is only supported for HTTP MCP servers"
                ))
            }
        };

        let oauth_cfg = cfg.oauth.as_ref();
        let scopes = oauth_cfg.map(|o| o.scopes.clone()).unwrap_or_default();
        let client_id = oauth_cfg.and_then(|o| o.client_id.clone());

        let tokens = run_oauth_flow(
            &self.http_client,
            server,
            &url,
            &scopes,
            client_id,
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

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Build a transport and initialize the connection for a single server.
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

    /// Build the appropriate `Transport` from server config.
    async fn build_transport(&self, name: &str, cfg: &McpServerConfig) -> Result<Transport> {
        match &cfg.transport {
            McpTransport::Stdio { command, args } => {
                let t = StdioTransport::spawn(command, args, &cfg.env, cfg.timeout_secs).await?;
                Ok(Transport::Stdio(Box::new(t)))
            }
            McpTransport::Http { url, headers } => {
                // Check for stored OAuth tokens.
                let auth = self.load_http_auth(name, url, cfg).await;
                let t = HttpTransport::new(url, headers, cfg.timeout_secs, auth)?;
                Ok(Transport::Http(t))
            }
        }
    }

    /// Load authentication state for an HTTP server.
    async fn load_http_auth(
        &self,
        name: &str,
        url: &str,
        cfg: &McpServerConfig,
    ) -> Option<AuthState> {
        // 1. Check for stored OAuth tokens.
        if let Some(mut stored) = self.store.load(name, url) {
            stored = ensure_fresh(&self.http_client, stored, &self.store).await;
            return Some(AuthState::OAuth {
                access_token: stored.access_token,
                refresh_token: stored.refresh_token,
                expires_at: stored.expires_at,
            });
        }

        // 2. Check for a bearer token in the configured headers.
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

/// Whether two server configs are meaningfully different (would require reconnect).
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
    // Compare transport by serialized form.
    let old_t = serde_json::to_string(&old.transport).unwrap_or_default();
    let new_t = serde_json::to_string(&new.transport).unwrap_or_default();
    old_t != new_t
}
