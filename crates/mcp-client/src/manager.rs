// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Multi-server MCP connection manager.
//!
//! `McpManager` owns all live server connections, handles connect/disconnect
//! lifecycle, exposes a unified tool and prompt catalogue, and drives the
//! reconnection loop. Requests run on a clone of a server's connection,
//! taken under the lock and used after it is released, so a slow tool call
//! never holds up a reconnect, a sign-in or another server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rmcp::model::{CallToolResult, Prompt, Tool};
use serde_json::Value;
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::auth::{self, TokenStore};
use crate::bridge::{McpPromptArgInfo, McpPromptInfo, McpTool};
use crate::config::{McpServerConfig, McpToolPolicy, McpTransport};
use crate::connection::{AuthRequired, Connection, Endpoint, ListChanged};
use crate::events::McpEvent;

mod sign_in;
use crate::health::{HealthState, ServerStatus, ServerStatusSummary};

use sven_tool_api::Tool as _;

// ── ServerState ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct ServerState {
    connection: Option<Connection>,
    health: HealthState,
    tools: Vec<Tool>,
    prompts: Vec<Prompt>,
    /// Set while a sign-in is in progress, so a second 401 arriving before
    /// the first sign-in completes does not open the browser twice.
    auth_in_progress: bool,
}

impl ServerState {
    fn new_disabled() -> Self {
        let mut state = Self::default();
        state.health.status = ServerStatus::Disabled;
        state
    }
}

// ── McpManager ────────────────────────────────────────────────────────────────

/// Multi-server MCP connection manager.
pub struct McpManager {
    config: RwLock<HashMap<String, McpServerConfig>>,
    servers: RwLock<HashMap<String, ServerState>>,
    store: Arc<TokenStore>,
    /// Notices for whoever shows them; sent without waiting ([`Self::emit`]).
    event_tx: mpsc::Sender<McpEvent>,
    /// How many times the tools changed: what a session follows to keep its
    /// tools current ([`Self::tool_changes`]), which no slow watcher can
    /// hold up or make it miss.
    tool_generation: watch::Sender<u64>,
    /// When false (headless/CI mode), never trigger interactive OAuth flows.
    /// Batch runs must complete without user interaction.
    allow_interactive_oauth: bool,
    /// Servers' `list_changed` notifications.
    changed_tx: mpsc::Sender<(String, ListChanged)>,
    changed_rx: Mutex<Option<mpsc::Receiver<(String, ListChanged)>>>,
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
        Self::with_token_store(
            config,
            event_tx,
            allow_interactive_oauth,
            TokenStore::new(TokenStore::default_path()),
        )
    }

    /// Like [`Self::new`], keeping sign-ins in `store`.
    pub fn with_token_store(
        config: HashMap<String, McpServerConfig>,
        event_tx: mpsc::Sender<McpEvent>,
        allow_interactive_oauth: bool,
        store: TokenStore,
    ) -> Arc<Self> {
        let (changed_tx, changed_rx) = mpsc::channel(32);
        Arc::new(Self {
            config: RwLock::new(config),
            servers: RwLock::new(HashMap::new()),
            store: Arc::new(store),
            event_tx,
            tool_generation: watch::Sender::new(0),
            allow_interactive_oauth,
            changed_tx,
            changed_rx: Mutex::new(Some(changed_rx)),
        })
    }

    /// Tells whoever watches about `event` without ever waiting on them: a
    /// watcher that does not keep up misses notices, and never holds up a
    /// connection. That the tools changed is also counted where
    /// [`Self::tool_changes`] sees it, which no full queue can lose.
    fn emit(&self, event: McpEvent) {
        if matches!(event, McpEvent::ToolsChanged) {
            self.tool_generation
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        if let Err(mpsc::error::TrySendError::Full(_)) = self.event_tx.try_send(event) {
            debug!("an MCP notice was dropped: its watcher is not keeping up");
        }
    }

    /// A count of the times the servers' tools changed, to follow: it moves
    /// whenever a server connects, refreshes its list or goes away, and the
    /// latest count is never lost, however late it is read. Ends with the
    /// manager.
    #[must_use]
    pub fn tool_changes(&self) -> watch::Receiver<u64> {
        self.tool_generation.subscribe()
    }

    // ── Background tasks ──────────────────────────────────────────────────────

    /// Start background tasks: list refreshes and the reconnection loop.
    ///
    /// Call once after `connect_all()`. The loop periodically checks for
    /// servers in the `Reconnecting` state and retries them when the backoff
    /// delay has elapsed.
    pub fn start_background_tasks(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let Some(mut rx) = this.changed_rx.lock().await.take() else {
                return;
            };
            while let Some((server, what)) = rx.recv().await {
                if let Err(e) = this.refresh(&server, what).await {
                    debug!(server = %server, error = %format!("{e:#}"), "refresh after list_changed failed");
                }
            }
        });

        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let due: Vec<String> = {
                    let servers = this.servers.read().await;
                    servers
                        .iter()
                        .filter(|(_, state)| state.health.should_retry_now())
                        .map(|(name, _)| name.clone())
                        .collect()
                };
                for name in due {
                    let this = Arc::clone(&this);
                    tokio::spawn(async move {
                        debug!(server = %name, "background reconnect attempt");
                        if let Err(e) = this.connect(&name).await {
                            debug!(server = %name, error = %e, "background reconnect failed");
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
                    warn!(server = %name, "MCP server connect failed: {e:#}");
                }
            });
        }
    }

    /// Connect to a single named server (by name from config).
    pub async fn connect(self: &Arc<Self>, name: &str) -> Result<()> {
        let cfg = self.server_config(name).await?;
        if !cfg.enabled {
            self.servers
                .write()
                .await
                .entry(name.to_string())
                .or_insert_with(ServerState::new_disabled);
            return Ok(());
        }
        self.servers
            .write()
            .await
            .entry(name.to_string())
            .or_default()
            .health
            .status = ServerStatus::Connecting;

        let timeout = Duration::from_secs(if cfg.timeout_secs == 0 {
            30
        } else {
            cfg.timeout_secs
        });
        let (endpoint, signed_in) = match &cfg.transport {
            McpTransport::Stdio { command, args } => (
                Endpoint::Stdio {
                    command,
                    args,
                    env: &cfg.env,
                },
                false,
            ),
            McpTransport::Http { url, headers } if cfg.token_exchange.is_some() => (
                Endpoint::Http {
                    url,
                    headers,
                    auth: None,
                    exchange: cfg.token_exchange.clone(),
                },
                false,
            ),
            McpTransport::Http { url, headers } => {
                let key = TokenStore::key(name, url);
                let client = match auth::signed_in_client(&self.store, &key, url).await {
                    Ok(client) => client,
                    Err(e) => {
                        // Not signed out: the sign-in could not be read (the
                        // server's authorization server is unreachable, or
                        // the store is damaged). Say so and try again later
                        // rather than starting a sign-in nobody asked for.
                        let error = format!(
                            "cannot use the stored sign-in: {e:#} \
                             (`sven mcp logout {name}` forgets it)"
                        );
                        warn!(server = %name, error = %error, "stored MCP sign-in unusable");
                        self.servers
                            .write()
                            .await
                            .entry(name.to_string())
                            .or_default()
                            .health
                            .report_error(error.clone());
                        self.emit(McpEvent::ServerFailed {
                            name: name.to_string(),
                            error,
                        });
                        return Err(e);
                    }
                };
                // A server configured for OAuth and never signed in to is
                // signed in to first: some refuse every unauthenticated
                // request with a 400 rather than a 401 challenge.
                if client.is_none() && cfg.oauth.is_some() {
                    self.needs_sign_in(name, &cfg, None, false);
                    return Ok(());
                }
                let signed_in = client.is_some();
                (
                    Endpoint::Http {
                        url,
                        headers,
                        auth: client,
                        exchange: None,
                    },
                    signed_in,
                )
            }
        };

        let opened = Connection::open(name, endpoint, timeout, self.changed_tx.clone()).await;
        let result = match opened {
            Ok(connection) => {
                let tools = connection.list_tools().await.unwrap_or_else(|e| {
                    warn!(server = %name, error = %format!("{e:#}"), "tools/list failed");
                    vec![]
                });
                let prompts = connection.list_prompts().await.unwrap_or_else(|e| {
                    debug!(server = %name, error = %format!("{e:#}"), "prompts/list failed");
                    vec![]
                });
                Ok((connection, tools, prompts))
            }
            Err(e) => Err(e),
        };
        match result {
            Ok((connection, tools, prompts)) => {
                let (tc, pc) = (tools.len(), prompts.len());
                {
                    let mut servers = self.servers.write().await;
                    let state = servers.entry(name.to_string()).or_default();
                    state.connection = Some(connection);
                    state.tools = tools;
                    state.prompts = prompts;
                    state.health.report_ok(tc, pc);
                }
                info!(server = %name, tools = tc, prompts = pc, "MCP server connected");
                self.emit(McpEvent::ServerConnected(name.to_string()));
                self.emit(McpEvent::ToolsChanged);
                Ok(())
            }
            Err(e) => {
                if cfg.token_exchange.is_some() {
                    // Tokens are traded for this machine's own sign-in: a
                    // refusal means that sign-in is gone, which a browser
                    // sign-in to this server cannot fix.
                    let error = format!(
                        "{e:#}; if this machine's sign-in was revoked, run `sven auth login` again"
                    );
                    self.servers
                        .write()
                        .await
                        .entry(name.to_string())
                        .or_default()
                        .health
                        .report_error(error.clone());
                    self.emit(McpEvent::ServerFailed {
                        name: name.to_string(),
                        error,
                    });
                } else if let Some(challenge) = e.downcast_ref::<AuthRequired>() {
                    self.needs_sign_in(name, &cfg, challenge.www_authenticate.clone(), signed_in);
                } else if is_auth_required_error(&e) {
                    self.needs_sign_in(name, &cfg, None, signed_in);
                } else {
                    let error = format!("{e:#}");
                    self.servers
                        .write()
                        .await
                        .entry(name.to_string())
                        .or_default()
                        .health
                        .report_error(error.clone());
                    self.emit(McpEvent::ServerFailed {
                        name: name.to_string(),
                        error,
                    });
                }
                Err(e)
            }
        }
    }

    /// Disconnect and remove a server.
    pub async fn disconnect(&self, name: &str) {
        if let Some(state) = self.servers.write().await.get_mut(name) {
            state.connection = None;
            state.tools.clear();
            state.prompts.clear();
            state.health.status = ServerStatus::Disabled;
        }
        self.emit(McpEvent::ServerDisconnected(name.to_string()));
        self.emit(McpEvent::ToolsChanged);
    }

    /// How many tools server `name` offers now (none while not connected).
    pub async fn tool_count(&self, name: &str) -> usize {
        self.servers
            .read()
            .await
            .get(name)
            .filter(|state| state.health.status.is_connected())
            .map_or(0, |state| state.tools.len())
    }

    /// Connect server `name` again every `every` until it offers more than
    /// `before` tools or `within` has passed: for an account the person is
    /// connecting in their browser, whose tools the server offers once they
    /// finish. Whether it came to offer more.
    pub async fn reconnect_until_more_tools(
        self: &Arc<Self>,
        name: &str,
        before: usize,
        every: Duration,
        within: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(every).await;
            if self.connect(name).await.is_ok() && self.tool_count(name).await > before {
                return true;
            }
        }
        false
    }

    // ── Tool access ───────────────────────────────────────────────────────────

    /// Every tool of every connected server the configuration does not
    /// deny, each asking or not as its policy says
    /// ([`crate::config::tool_policy`]).
    pub async fn tools(self: &Arc<Self>) -> Vec<McpTool> {
        let config = self.config.read().await;
        let servers = self.servers.read().await;
        let mut tools = Vec::new();
        for (name, state) in servers.iter() {
            if !state.health.status.is_connected() {
                continue;
            }
            let policies = config.get(name).map(|c| &c.tools);
            for t in &state.tools {
                // The account's copy of a service configured here is
                // dropped: the local server is the higher tier.
                if crate::dedupe::duplicates_local(t.meta.as_ref(), name, &config) {
                    continue;
                }
                let destructive = t
                    .annotations
                    .as_ref()
                    .and_then(|a| a.destructive_hint)
                    .unwrap_or(false);
                let policy = policies.map_or(McpToolPolicy::Ask, |p| {
                    crate::config::tool_policy(p, &t.name, destructive)
                });
                if policy == McpToolPolicy::Deny {
                    continue;
                }
                tools.push(McpTool::new(
                    name.clone(),
                    t.name.to_string(),
                    t.description.as_deref().map(str::to_string),
                    Value::Object((*t.input_schema).clone()),
                    Arc::clone(self),
                    policy == McpToolPolicy::Allow,
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
                let arguments = p
                    .arguments
                    .iter()
                    .flatten()
                    .map(|a| McpPromptArgInfo {
                        name: a.name.clone(),
                        description: a.description.clone().unwrap_or_default(),
                        required: a.required.unwrap_or(false),
                    })
                    .collect();
                prompts.push(McpPromptInfo {
                    slash_path: format!("{}/{}", name, p.name),
                    server_name: name.clone(),
                    prompt_name: p.name.clone(),
                    description: p.description.clone().unwrap_or_default(),
                    arguments,
                });
            }
        }
        prompts.sort_by(|a, b| a.slash_path.cmp(&b.slash_path));
        prompts
    }

    // ── Tool execution ────────────────────────────────────────────────────────

    /// The live connection to `server`, cloned so the caller holds no lock.
    async fn connection(&self, server: &str) -> Result<Connection> {
        self.servers
            .read()
            .await
            .get(server)
            .with_context(|| format!("no MCP server state for {server}"))?
            .connection
            .clone()
            .with_context(|| format!("MCP server {server} is not connected"))
    }

    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        args: &Value,
    ) -> Result<CallToolResult> {
        self.connection(server).await?.call_tool(tool, args).await
    }

    pub async fn get_prompt(
        &self,
        server: &str,
        prompt: &str,
        args: HashMap<String, String>,
    ) -> Result<String> {
        self.connection(server)
            .await?
            .get_prompt(prompt, &args)
            .await
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
                let (status, tool_count, prompt_count) = servers.get(&name).map_or_else(
                    || {
                        let status = match config.get(&name) {
                            Some(c) if !c.enabled => ServerStatus::Disabled,
                            _ => ServerStatus::Initializing,
                        };
                        (status, 0, 0)
                    },
                    |s| {
                        (
                            s.health.status.clone(),
                            s.health.tool_count,
                            s.health.prompt_count,
                        )
                    },
                );
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
            let to_remove: Vec<String> = current
                .keys()
                .filter(|name| !new_config.contains_key(*name))
                .cloned()
                .collect();
            let mut to_add = Vec::new();
            let mut to_update = Vec::new();
            for (name, new_cfg) in &new_config {
                match current.get(name) {
                    None => to_add.push(name.clone()),
                    Some(old_cfg) if config_changed(old_cfg, new_cfg) => {
                        to_update.push(name.clone());
                    }
                    Some(_) => {}
                }
            }
            (to_remove, to_add, to_update)
        };
        *self.config.write().await = new_config;

        for name in to_remove {
            self.disconnect(&name).await;
        }
        for name in &to_update {
            self.disconnect(name).await;
        }
        for name in to_update.into_iter().chain(to_add) {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(e) = this.connect(&name).await {
                    warn!(server = %name, error = %e, "MCP server connect failed");
                }
            });
        }
    }

    async fn server_config(&self, server: &str) -> Result<McpServerConfig> {
        self.config
            .read()
            .await
            .get(server)
            .cloned()
            .with_context(|| format!("no MCP server config for {server}"))
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Refetch what a server's `list_changed` notification named, on a clone
    /// of its connection, and announce the change.
    async fn refresh(self: &Arc<Self>, server: &str, what: ListChanged) -> Result<()> {
        let connection = self.connection(server).await?;
        let (tools, prompts) = match what {
            ListChanged::Tools => (Some(connection.list_tools().await?), None),
            ListChanged::Prompts => (None, Some(connection.list_prompts().await?)),
        };
        let (tc, pc) = {
            let mut servers = self.servers.write().await;
            let state = servers
                .get_mut(server)
                .with_context(|| format!("no MCP server state for {server}"))?;
            if let Some(tools) = tools {
                state.tools = tools;
            }
            if let Some(prompts) = prompts {
                state.prompts = prompts;
            }
            state
                .health
                .report_ok(state.tools.len(), state.prompts.len());
            (state.tools.len(), state.prompts.len())
        };
        info!(server = %server, tools = tc, prompts = pc, "refreshed after list_changed");
        self.emit(McpEvent::ToolsChanged);
        Ok(())
    }
}

// ── Config change detection ───────────────────────────────────────────────────

/// Build an error message when the server rejects credentials after OAuth.
/// Includes Atlassian-specific hint when applicable (admin allowlist requirement).
fn build_rejected_credentials_error(server_url: &str) -> String {
    let base = format!(
        "Server rejected credentials after authentication: {server_url}. \
         OAuth completed successfully but the MCP server returned 401."
    );
    if server_url.contains("mcp.atlassian.com") {
        format!(
            "{base} For Atlassian MCP, an admin must allowlist your client's domain \
             (e.g. http://127.0.0.1:*/** for local apps) in Rovo MCP Server settings. \
             See: https://support.atlassian.com/security-and-access-policies/docs/available-atlassian-rovo-mcp-server-domains/"
        )
    } else {
        base
    }
}

/// Check if an error indicates the server requires authentication without
/// saying so in the standard way: a 400 (Atlassian, for unauthenticated
/// requests) or a 401 with no challenge.
fn is_auth_required_error(e: &anyhow::Error) -> bool {
    let s = format!("{:#}", e);
    s.contains("Request must be an initialize request if no session ID is provided")
        || s.contains("No valid session ID provided")
        // A 401 without the `WWW-Authenticate` challenge the spec requires.
        || s.contains("HTTP 401")
}

fn config_changed(old: &McpServerConfig, new: &McpServerConfig) -> bool {
    if old.enabled != new.enabled {
        return true;
    }
    if old.timeout_secs != new.timeout_secs {
        return true;
    }
    if old.env != new.env || old.tools != new.tools {
        return true;
    }
    let old_t = serde_json::to_string(&old.transport).unwrap_or_default();
    let new_t = serde_json::to_string(&new.transport).unwrap_or_default();
    old_t != new_t
}
