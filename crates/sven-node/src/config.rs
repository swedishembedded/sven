// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Gateway configuration loaded from YAML.
//!
//! Configuration is YAML (never TOML).  Layers are **deep-merged** — you can
//! override only the fields you care about in each file.
//!
//! Search order (later overrides earlier):
//! 1. `/etc/sven/gateway.yaml`
//! 2. `~/.config/sven/gateway.yaml`
//! 3. `.sven/gateway.yaml` (workspace-local)
//! 4. Path given to [`load`] explicitly.
//!
//! **All defaults are production-safe.** Running `load(None)` with no config
//! file gives you TLS on, loopback bind, no Slack, P2P on a random port.
//!
//! # Loading
//!
//! ```rust
//! use sven_node::config::{GatewayConfig, load};
//!
//! // Load from the default search paths (no explicit file).
//! let config = load(None).unwrap();
//!
//! // Defaults are secure.
//! assert!(!config.http.insecure_dev_mode);     // TLS is on
//! assert!(config.http.bind.starts_with("127.0.0.1")); // loopback only
//! assert!(config.p2p.mdns);                    // mDNS for LAN discovery
//! ```
//!
//! # Example full config
//! ```yaml
//! http:
//!   bind: "127.0.0.1:18790"
//!   # TLS is on by default. Set insecure_dev_mode: true ONLY for local development.
//!   insecure_dev_mode: false
//!   token_file: "~/.config/sven/gateway-token.yaml"
//!
//! p2p:
//!   listen: "/ip4/0.0.0.0/tcp/0"
//!   keypair_path: "~/.config/sven/gateway-keypair"
//!   authorized_peers_file: "~/.config/sven/authorized_peers.yaml"
//!
//! slack:
//!   accounts:
//!     - mode: socket          # "socket" or "http"
//!       app_token: "xapp-..."
//!       bot_token: "xoxb-..."
//! ```

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::debug;

fn default_http_bind() -> String {
    "127.0.0.1:18790".to_string()
}
fn default_true() -> bool {
    true
}

/// Top-level gateway configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub p2p: P2pGatewayConfig,
    #[serde(default)]
    pub slack: SlackConfig,
}

/// HTTP/WebSocket listener configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpConfig {
    /// `host:port` to listen on. Default: `127.0.0.1:18790` (loopback only).
    #[serde(default = "default_http_bind")]
    pub bind: String,

    /// TLS is **enabled by default**. Set this to `true` only for local
    /// development. The flag is intentionally named to make it uncomfortable
    /// to leave on in production.
    #[serde(default)]
    pub insecure_dev_mode: bool,

    /// Directory where the auto-generated ECDSA P-256 certificate and private
    /// key are stored. Defaults to `~/.config/sven/gateway/tls/`.
    pub tls_cert_dir: Option<PathBuf>,

    /// Path to the YAML file that stores the SHA-256 hashed HTTP bearer token.
    /// If `None`, the token file is auto-located at
    /// `~/.config/sven/gateway/token.yaml`.
    pub token_file: Option<PathBuf>,

    /// Maximum request body size in bytes (default: 4 MiB).
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
}

fn default_max_body() -> usize {
    4 * 1024 * 1024
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: default_http_bind(),
            insecure_dev_mode: false,
            tls_cert_dir: None,
            token_file: None,
            max_body_bytes: default_max_body(),
        }
    }
}

/// libp2p P2P listener configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct P2pGatewayConfig {
    /// libp2p listen address for the **operator control channel**.
    ///
    /// This is the port used by mobile/native operator clients that pair via
    /// `sven node authorize`.  It does **not** handle agent-to-agent traffic.
    ///
    /// Default: `/ip4/0.0.0.0/tcp/0` (OS-assigned port).
    /// Recommended for cross-machine use: set a fixed port and open it in your
    /// firewall, e.g. `/ip4/0.0.0.0/tcp/4009`.
    #[serde(default = "default_p2p_listen")]
    pub listen: String,

    /// libp2p listen address for the **agent-to-agent mesh**.
    ///
    /// This is the port other sven agents dial when they connect to this node
    /// for task delegation.  It is separate from the operator control port.
    ///
    /// Default: `/ip4/0.0.0.0/tcp/0` (OS-assigned random port).
    /// **Must be set to a fixed port and opened in your firewall** when nodes
    /// run on different machines, e.g. `/ip4/0.0.0.0/tcp/4010`.
    #[serde(default = "default_agent_listen")]
    pub agent_listen: String,

    /// Path for persisting the gateway's Ed25519 keypair. When absent a new
    /// keypair is generated each run (ephemeral identity — mobile operators
    /// would need to re-pair after each restart).
    pub keypair_path: Option<PathBuf>,

    /// YAML file listing authorized peer IDs and their roles.
    /// See [`crate::p2p::auth::PeerAllowlist`].
    /// Default: `~/.config/sven/gateway/authorized_peers.yaml`
    pub authorized_peers_file: Option<PathBuf>,

    /// mDNS local-network discovery is **enabled by default** so nearby
    /// mobile clients can find the gateway without configuration.
    #[serde(default = "default_true")]
    pub mdns: bool,

    /// Identity advertised to other agents over the task-routing P2P channel.
    /// If omitted, a default card is generated from the system hostname.
    #[serde(default)]
    pub agent: AgentIdentityConfig,

    /// Rooms this agent participates in for agent-to-agent task routing.
    /// A "room" is a named discovery namespace; peers in the same room find
    /// each other via mDNS (LAN) or relay (WAN).
    /// Default: `["default"]`.
    #[serde(default = "default_rooms")]
    pub rooms: Vec<String>,

    /// Path for persisting the agent P2P keypair (separate from the operator
    /// control keypair).  Defaults to
    /// `~/.config/sven/gateway/agent-keypair`.
    pub agent_keypair_path: Option<PathBuf>,

    /// Agent peers allowed to join this node's mesh.
    ///
    /// Maps peer ID (base58 string) → human-readable label.  **An empty map
    /// (the default) means deny-all** — no remote agent can connect until at
    /// least one entry is added here.
    ///
    /// Get the peer ID from the other node's startup log:
    ///
    /// ```text
    /// P2pNode starting peer_id=12D3KooW…
    /// ```
    ///
    /// Example config:
    ///
    /// ```yaml
    /// p2p:
    ///   peers:
    ///     "12D3KooWAbCdEfGhIjKlMnOpQrStUvWxYz": "machine-b"
    ///     "12D3KooWXyZaBcDeFgHiJkLmNo12345678": "machine-c"
    /// ```
    ///
    /// Both nodes must list each other — authorization is not automatic.
    #[serde(default)]
    pub peers: HashMap<String, String>,
}

/// Human-readable identity broadcast to other agents when they connect.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentityConfig {
    /// Short display name, e.g. `"backend-agent"`.
    /// Defaults to the system hostname.
    pub name: Option<String>,
    /// Free-form description of this agent's expertise.
    pub description: Option<String>,
    /// Capability tags for peer discovery, e.g. `["rust", "backend", "postgres"]`.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn default_p2p_listen() -> String {
    "/ip4/0.0.0.0/tcp/0".to_string()
}

fn default_agent_listen() -> String {
    "/ip4/0.0.0.0/tcp/0".to_string()
}

fn default_rooms() -> Vec<String> {
    vec!["default".to_string()]
}

impl Default for P2pGatewayConfig {
    fn default() -> Self {
        Self {
            listen: default_p2p_listen(),
            agent_listen: default_agent_listen(),
            keypair_path: None,
            authorized_peers_file: None,
            mdns: true,
            agent: AgentIdentityConfig::default(),
            rooms: default_rooms(),
            agent_keypair_path: None,
            peers: HashMap::new(),
        }
    }
}

/// Slack integration configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlackConfig {
    /// One entry per Slack workspace / bot token.
    #[serde(default)]
    pub accounts: Vec<SlackAccount>,
}

/// Configuration for a single Slack bot account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackAccount {
    /// `socket` (outbound WebSocket, default) or `http` (inbound webhook).
    #[serde(default = "default_slack_mode")]
    pub mode: SlackMode,

    /// Slack App-level token (`xapp-…`). Required for Socket Mode.
    pub app_token: Option<String>,

    /// Slack Bot token (`xoxb-…`). Required for both modes.
    pub bot_token: Option<String>,

    /// Slack signing secret — used to verify HMAC-SHA256 signatures on
    /// incoming HTTP events. Required when `mode = http`.
    pub signing_secret: Option<String>,

    /// Webhook path for HTTP mode. Default: `/slack/events`.
    #[serde(default = "default_slack_webhook_path")]
    pub webhook_path: String,
}

fn default_slack_mode() -> SlackMode {
    SlackMode::Socket
}
fn default_slack_webhook_path() -> String {
    "/slack/events".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlackMode {
    /// Outbound WebSocket connection to Slack — no inbound port needed.
    Socket,
    /// Inbound HTTP webhook — Slack POSTs events to your server.
    Http,
}

// ── Loader ────────────────────────────────────────────────────────────────────

fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(PathBuf::from("/etc/sven/gateway.yaml"));
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config/sven/gateway.yaml"));
    }
    paths.push(PathBuf::from(".sven/gateway.yaml"));
    paths
}

pub fn load(extra: Option<&Path>) -> anyhow::Result<GatewayConfig> {
    let mut merged = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());

    for path in config_search_paths() {
        if path.is_file() {
            debug!(path = %path.display(), "loading gateway config layer");
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let layer: serde_yaml::Value = serde_yaml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            merge_yaml(&mut merged, layer);
        }
    }

    if let Some(p) = extra {
        debug!(path = %p.display(), "loading explicit gateway config");
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let layer: serde_yaml::Value =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        merge_yaml(&mut merged, layer);
    }

    let config: GatewayConfig = if matches!(&merged, serde_yaml::Value::Mapping(m) if m.is_empty())
    {
        GatewayConfig::default()
    } else {
        serde_yaml::from_value(merged).unwrap_or_default()
    };
    Ok(config)
}

fn merge_yaml(dst: &mut serde_yaml::Value, src: serde_yaml::Value) {
    match (dst, src) {
        (serde_yaml::Value::Mapping(d), serde_yaml::Value::Mapping(s)) => {
            for (k, v) in s {
                let entry = d
                    .entry(k)
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                merge_yaml(entry, v);
            }
        }
        (dst, src) => *dst = src,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_http_bind_is_loopback() {
        let c = GatewayConfig::default();
        assert!(
            c.http.bind.starts_with("127.0.0.1"),
            "default must be loopback-only"
        );
    }

    #[test]
    fn default_tls_is_enabled() {
        let c = GatewayConfig::default();
        assert!(!c.http.insecure_dev_mode, "TLS must be on by default");
    }

    #[test]
    fn default_mdns_is_enabled() {
        let c = GatewayConfig::default();
        assert!(c.p2p.mdns, "mDNS must be on by default");
    }

    #[test]
    fn default_slack_mode_is_socket() {
        let account = SlackAccount {
            mode: default_slack_mode(),
            app_token: None,
            bot_token: None,
            signing_secret: None,
            webhook_path: default_slack_webhook_path(),
        };
        assert_eq!(account.mode, SlackMode::Socket);
    }

    #[test]
    fn config_yaml_round_trip() {
        let c = GatewayConfig::default();
        let yaml = serde_yaml::to_string(&c).unwrap();
        let back: GatewayConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.http.bind, c.http.bind);
        assert_eq!(back.http.insecure_dev_mode, c.http.insecure_dev_mode);
    }

    #[test]
    fn config_insecure_dev_mode_can_be_set() {
        let yaml = "http:\n  insecure_dev_mode: true\n";
        let c: GatewayConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(c.http.insecure_dev_mode);
    }

    #[test]
    fn load_returns_defaults_when_no_files_exist() {
        let c = load(None).unwrap();
        assert_eq!(c.http.bind, default_http_bind());
    }
}
