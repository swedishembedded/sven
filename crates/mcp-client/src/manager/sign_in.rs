// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a person does to a server: sign in (when the server asks, or on
//! request: `/mcp auth`, `sven mcp auth`), sign out, enable or disable it,
//! and add or remove it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::{build_rejected_credentials_error, McpManager};
use crate::auth::{self, SignIn, TokenStore};
use crate::config::{McpServerConfig, McpTransport};
use crate::events::McpEvent;
use crate::health::ServerStatus;

impl McpManager {
    /// The server needs a sign-in: it answered 401 (`challenge` is its
    /// `WWW-Authenticate`), or it is configured for OAuth and has none.
    ///
    /// When `signed_in` is true the server refused the sign-in sven sent; the
    /// sign-in is forgotten and the server reported failed, rather than
    /// opening the browser in a loop. Otherwise an interactive session signs
    /// in now (once, however many 401s arrive meanwhile) and reconnects; a
    /// headless one reports [`McpEvent::AuthRequired`] and never waits for a
    /// person.
    pub(super) fn needs_sign_in(
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
                        auth_url: String::new(),
                    };
                }
                this.emit(McpEvent::AuthRequired {
                    server: name,
                    auth_url: url,
                });
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
                    auth_url: String::new(),
                };
            }

            if !this.allow_interactive_oauth {
                this.emit(McpEvent::AuthRequired {
                    server: name,
                    auth_url: url,
                });
                return;
            }

            this.emit(McpEvent::AuthStarted {
                server: name.clone(),
            });
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
                this.emit(McpEvent::ServerFailed { name, error });
            }
        });
    }

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

    /// Wait for `server` to be connected, which a sign-in in progress
    /// brings about once the person finishes it; its tool count. Fails with
    /// the reason when the server fails, or needs a sign-in nobody is
    /// doing, or when `within` passes.
    pub async fn wait_connected(&self, server: &str, within: Duration) -> Result<usize> {
        // A finished sign-in reconnects from a task of its own, so for a
        // moment the server still reads as needing a sign-in nobody is
        // doing: that state has to hold for a few polls to be believed.
        const POLL: Duration = Duration::from_millis(500);
        const SETTLE_POLLS: u32 = 6;
        let deadline = tokio::time::Instant::now() + within;
        let mut idle = 0;
        loop {
            {
                let servers = self.servers.read().await;
                if let Some(state) = servers.get(server) {
                    match &state.health.status {
                        ServerStatus::Connected => return Ok(state.tools.len()),
                        ServerStatus::Failed { error } => anyhow::bail!("{error}"),
                        ServerStatus::NeedsAuth { .. } if !state.auth_in_progress => idle += 1,
                        _ => idle = 0,
                    }
                }
            }
            if idle >= SETTLE_POLLS {
                anyhow::bail!("{server} needs a sign-in that is not in progress");
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for {server} to connect");
            }
            tokio::time::sleep(POLL).await;
        }
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
        let this = Arc::clone(self);
        let present = move |url: &str| {
            crate::callback::open_browser(url);
            let _ = events.try_send(McpEvent::SignInLink {
                server: name.clone(),
                url: url.to_string(),
            });
            // Kept on the server's status too, so `/mcp` shows the whole
            // link for as long as the sign-in waits for it.
            let (this, name, url) = (Arc::clone(&this), name.clone(), url.to_string());
            tokio::spawn(async move {
                if let Some(state) = this.servers.write().await.get_mut(&name) {
                    state.health.status = ServerStatus::NeedsAuth { auth_url: url };
                }
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
                name: server,
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
}
