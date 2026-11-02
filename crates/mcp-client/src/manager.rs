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
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::auth::{self, SignIn, TokenStore};
use crate::bridge::{McpPromptArgInfo, McpPromptInfo, McpTool};
use crate::config::{McpServerConfig, McpToolPolicy, McpTransport};
use crate::connection::{AuthRequired, Connection, Endpoint, ListChanged};
use crate::events::McpEvent;
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
    event_tx: mpsc::Sender<McpEvent>,
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
            allow_interactive_oauth,
            changed_tx,
            changed_rx: Mutex::new(Some(changed_rx)),
        })
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
                        warn!(server = %name, error = %format!("{e:#}"), "stored MCP sign-in unusable; signing in again");
                        None
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
                let _ = self
                    .event_tx
                    .send(McpEvent::ServerConnected(name.to_string()))
                    .await;
                let _ = self.event_tx.send(McpEvent::ToolsChanged).await;
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
                    let _ = self
                        .event_tx
                        .send(McpEvent::ServerFailed {
                            name: name.to_string(),
                            error,
                        })
                        .await;
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
                    let _ = self
                        .event_tx
                        .send(McpEvent::ServerFailed {
                            name: name.to_string(),
                            error,
                        })
                        .await;
                }
                Err(e)
            }
        }
    }

    /// The server needs a sign-in: it answered 401 (`challenge` is its
    /// `WWW-Authenticate`), or it is configured for OAuth and has none.
    ///
    /// When `signed_in` is true the server refused the sign-in sven sent; the
    /// sign-in is forgotten and the server reported failed, rather than
    /// opening the browser in a loop. Otherwise an interactive session signs
    /// in now (once, however many 401s arrive meanwhile) and reconnects; a
    /// headless one reports [`McpEvent::AuthRequired`] and never waits for a
    /// person.
    fn needs_sign_in(
        self: &Arc<Self>,
        name: &str,
        cfg: &McpServerConfig,
        challenge: Option<String>,
        signed_in: bool,
    ) {
        let McpTransport::Http { url, .. } = &cfg.transport else {
            return;
        };
        let url = url.clone();
        let name = name.to_string();
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if signed_in {
                if let Err(e) = this.store.remove(&TokenStore::key(&name, &url)) {
                    warn!(server = %name, error = %format!("{e:#}"), "could not forget the refused MCP sign-in");
                }
                warn!(server = %name, "{}", build_rejected_credentials_error(&url));
                if let Some(state) = this.servers.write().await.get_mut(&name) {
                    state.auth_in_progress = false;
                    state.health.status = ServerStatus::NeedsAuth {
                        auth_url: url.clone(),
                    };
                }
                let _ = this
                    .event_tx
                    .send(McpEvent::AuthRequired {
                        server: name,
                        auth_url: url,
                    })
                    .await;
                return;
            }

            {
                let mut servers = this.servers.write().await;
                let state = servers.entry(name.clone()).or_default();
                if state.auth_in_progress {
                    debug!(server = %name, "sign-in already in progress");
                    return;
                }
                state.auth_in_progress = this.allow_interactive_oauth;
                state.health.status = ServerStatus::NeedsAuth {
                    auth_url: url.clone(),
                };
            }

            if !this.allow_interactive_oauth {
                let _ = this
                    .event_tx
                    .send(McpEvent::AuthRequired {
                        server: name,
                        auth_url: url,
                    })
                    .await;
                return;
            }

            let _ = this
                .event_tx
                .send(McpEvent::AuthStarted {
                    server: name.clone(),
                })
                .await;
            let signed = this.sign_in(&name, challenge.as_deref()).await;
            if let Some(state) = this.servers.write().await.get_mut(&name) {
                state.auth_in_progress = false;
            }
            if let Err(e) = signed {
                warn!(server = %name, error = %format!("{e:#}"), "MCP sign-in failed");
                let error = format!("{e:#}");
                if let Some(state) = this.servers.write().await.get_mut(&name) {
                    // Failed, not Reconnecting: the background loop must not
                    // open the browser again on its own.
                    state.health.status = ServerStatus::Failed {
                        error: error.clone(),
                    };
                }
                let _ = this
                    .event_tx
                    .send(McpEvent::ServerFailed { name, error })
                    .await;
            }
        });
    }

    /// Disconnect and remove a server.
    pub async fn disconnect(&self, name: &str) {
        if let Some(state) = self.servers.write().await.get_mut(name) {
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

    // ── Sign-in ───────────────────────────────────────────────────────────────

    /// Sign in to `server` in the browser (`/mcp auth <name>`), then
    /// reconnect it. Waits until the browser comes back (at most five
    /// minutes). Refused in a headless run.
    pub async fn authenticate(self: &Arc<Self>, server: &str) -> Result<()> {
        self.sign_in(server, None).await
    }

    /// Like [`Self::authenticate`], showing the sign-in page with `present`
    /// and also completing from an address the person pastes into `pasted`
    /// (a command line on a remote shell, whose browser is elsewhere).
    pub async fn authenticate_with(
        self: &Arc<Self>,
        server: &str,
        present: &(dyn Fn(&str) + Send + Sync),
        pasted: Option<mpsc::Receiver<String>>,
    ) -> Result<()> {
        self.sign_in_with(server, None, present, pasted).await
    }

    /// Connect `server` and offer its tools (`true`), or disconnect it and
    /// withdraw them (`false`), for the life of this manager; the
    /// configuration file is not changed.
    pub async fn set_enabled(self: &Arc<Self>, server: &str, enabled: bool) -> Result<()> {
        {
            let mut config = self.config.write().await;
            let cfg = config
                .get_mut(server)
                .with_context(|| format!("no MCP server config for {server}"))?;
            cfg.enabled = enabled;
        }
        if enabled {
            self.connect(server).await
        } else {
            self.disconnect(server).await;
            Ok(())
        }
    }

    /// Start using server `name` with `config` (just added to a
    /// configuration file) and connect it; an HTTP server that answers 401
    /// is signed in to as on any connection.
    pub async fn add_server(self: &Arc<Self>, name: &str, config: McpServerConfig) -> Result<()> {
        self.config.write().await.insert(name.to_string(), config);
        self.connect(name).await
    }

    /// Stop using server `name` (just removed from a configuration file).
    pub async fn remove_server(self: &Arc<Self>, name: &str) {
        self.disconnect(name).await;
        self.config.write().await.remove(name);
        self.servers.write().await.remove(name);
    }

    /// Forget the sign-in of `server`; it asks for a new one when it is
    /// next connected.
    pub async fn sign_out(self: &Arc<Self>, server: &str) -> Result<()> {
        let cfg = self.server_config(server).await?;
        let McpTransport::Http { url, .. } = &cfg.transport else {
            anyhow::bail!("{server} is a stdio server; it has no sign-in");
        };
        self.store.remove(&TokenStore::key(server, url))?;
        self.disconnect(server).await;
        Ok(())
    }

    async fn sign_in(self: &Arc<Self>, server: &str, challenge: Option<&str>) -> Result<()> {
        let events = self.event_tx.clone();
        let name = server.to_string();
        let present = move |url: &str| {
            crate::callback::open_browser(url);
            let _ = events.try_send(McpEvent::SignInLink {
                server: name.clone(),
                url: url.to_string(),
            });
        };
        self.sign_in_with(server, challenge, &present, None).await
    }

    async fn sign_in_with(
        self: &Arc<Self>,
        server: &str,
        challenge: Option<&str>,
        present: &(dyn Fn(&str) + Send + Sync),
        pasted: Option<mpsc::Receiver<String>>,
    ) -> Result<()> {
        if !self.allow_interactive_oauth {
            anyhow::bail!(
                "MCP sign-in cannot run in a headless or CI run. \
                 Sign in from an interactive session first (`/mcp auth {server}`)."
            );
        }
        let cfg = self.server_config(server).await?;
        let McpTransport::Http { url, .. } = &cfg.transport else {
            anyhow::bail!("{server} is a stdio server; only HTTP servers sign in");
        };
        auth::sign_in(
            &self.store,
            SignIn {
                key: &TokenStore::key(server, url),
                url,
                oauth: cfg.oauth.as_ref(),
                challenge,
                present,
                pasted,
            },
        )
        .await?;
        let this = Arc::clone(self);
        let name = server.to_string();
        tokio::spawn(async move {
            if let Err(e) = this.connect(&name).await {
                warn!(server = %name, error = %e, "reconnect after sign-in failed");
            }
        });
        Ok(())
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
        let _ = self.event_tx.send(McpEvent::ToolsChanged).await;
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
