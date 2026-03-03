// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Node configuration loaded from YAML.
//!
//! Configuration is YAML (never TOML).  Layers are **deep-merged** — you can
//! override only the fields you care about in each file.
//!
//! Search order (later overrides earlier):
//! 1. `/etc/sven/node.yaml`
//! 2. `~/.config/sven/node.yaml`
//! 3. `.sven/node.yaml` (workspace-local)
//! 4. Path given to [`load`] explicitly.
//!
//! **All defaults are production-safe.** Running `load(None)` with no config
//! file gives you TLS on, loopback bind, no Slack, P2P on a random port.
//!
//! # Loading
//!
//! ```rust
//! use sven_node::config::{NodeConfig, load};
//!
//! // Load from the default search paths (no explicit file).
//! let config = load(None).unwrap();
//!
//! // Defaults are secure.
//! assert!(!config.http.insecure_dev_mode);     // TLS is on
//! assert!(config.http.bind.starts_with("127.0.0.1")); // loopback only
//! assert!(config.control.is_none());           // control node off by default
//! assert!(config.swarm.peers.is_empty());      // agent mesh deny-all by default
//! ```
//!
//! # Example full config
//! ```yaml
//! http:
//!   bind: "127.0.0.1:18790"
//!   # TLS is on by default. Set insecure_dev_mode: true ONLY for local development.
//!   insecure_dev_mode: false
//!   token_file: "~/.config/sven/node/token.yaml"
//!
//! swarm:
//!   listen: "/ip4/0.0.0.0/tcp/4010"   # fixed port for agent mesh; open in firewall
//!   keypair_path: "~/.config/sven/node/agent-keypair"
//!
//! # Operator control node — omit to disable native/mobile access entirely
//! # control:
//! #   listen: "/ip4/0.0.0.0/tcp/4009"
//! #   keypair_path: "~/.config/sven/node/control-keypair"
//! #   authorized_peers_file: "~/.config/sven/node/authorized_peers.yaml"
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

// ── Tilde-expansion serde helpers ─────────────────────────────────────────────
//
// YAML deserialization of `PathBuf` stores the string as-is, so any path like
// `~/foo` ends up with a literal `~` rather than the home directory.  These
// helpers are attached via `#[serde(deserialize_with = "…")]` on every PathBuf
// field in the config structs below.

/// Expand a leading `~` to the current user's home directory.
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest)
    } else if s == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(s)
    }
}

/// Serde deserializer for `Option<PathBuf>` that expands a leading `~`.
fn de_opt_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.as_deref().map(expand_tilde))
}
use tracing::debug;

fn default_http_bind() -> String {
    "127.0.0.1:18790".to_string()
}

/// Top-level node configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeConfig {
    #[serde(default)]
    pub http: HttpConfig,

    /// Agent-to-agent mesh configuration.
    #[serde(default)]
    pub swarm: SwarmConfig,

    /// Operator control node (P2P channel for native/mobile clients).
    ///
    /// **Disabled by default** — omit this section entirely to run without a
    /// control node.  Only add it when you need to pair a native or mobile
    /// operator client via the `sven://` URI flow.
    ///
    /// The operator control channel is completely separate from the agent mesh
    /// (`swarm`): it carries human operator commands, not agent-to-agent tasks.
    #[serde(default)]
    pub control: Option<ControlConfig>,

    #[serde(default)]
    pub slack: SlackConfig,

    /// Browser-based web terminal (passkey auth + PTY sessions).
    ///
    /// When present, the node serves a web terminal at `/web` that allows
    /// browser-based access via WebAuthn passkeys (biometric auth).  New
    /// devices are held for admin approval before gaining PTY access.
    #[serde(default)]
    pub web: Option<WebConfig>,
}

/// TLS provisioning strategy.
///
/// # `auto` (default)
/// Try Tailscale first (if the `tailscale` CLI is present and the machine is
/// enrolled in a tailnet). If that fails, fall back to `local-ca`.
///
/// # `tailscale`
/// Use `tailscale cert` to fetch a Let's Encrypt certificate for the
/// machine's `*.ts.net` FQDN. These certs are trusted by all browsers with
/// zero additional setup. Requires Tailscale to be installed and running.
///
/// # `local-ca`
/// Generate a local ECDSA P-256 CA certificate (10-year validity) and sign a
/// 90-day server certificate with it. The CA cert is stored at
/// `<tls_cert_dir>/ca-cert.pem`. Trust it once with `sven node install-ca`
/// and every future server cert is automatically accepted by the browser.
///
/// # `self-signed`
/// Pure self-signed certificate — the existing behaviour. The browser will
/// always warn unless the cert is manually added as a trusted exception.
///
/// # `files`
/// Read `node-cert.pem` / `node-key.pem` from `tls_cert_dir` as-is.
/// Bring your own certificates (e.g. from an external ACME client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// Try Tailscale, fall back to local CA.
    #[default]
    Auto,
    /// `tailscale cert` — browser-trusted via Let's Encrypt.
    Tailscale,
    /// Local CA — trust once with `sven node install-ca`.
    LocalCa,
    /// Pure self-signed — browser warning every time.
    SelfSigned,
    /// User-supplied cert/key files in `tls_cert_dir`.
    Files,
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

    /// TLS certificate provisioning strategy.
    ///
    /// Default: `auto` — tries Tailscale, falls back to `local-ca`.
    /// See [`TlsMode`] for all options.
    #[serde(default)]
    pub tls_mode: TlsMode,

    /// Directory where TLS certificates are stored / generated.
    /// Defaults to `~/.config/sven/node/tls/`.
    #[serde(default, deserialize_with = "de_opt_path")]
    pub tls_cert_dir: Option<PathBuf>,

    /// Extra Subject Alternative Names to add to generated server certs.
    ///
    /// Use this to include your machine's LAN IP or custom hostname so the
    /// cert is valid when accessed from other machines:
    /// ```yaml
    /// http:
    ///   tls_san_extra: ["192.168.1.42", "mybox.local"]
    /// ```
    #[serde(default)]
    pub tls_san_extra: Vec<String>,

    /// Path to the YAML file that stores the SHA-256 hashed HTTP bearer token.
    /// If `None`, the token file is auto-located at
    /// `~/.config/sven/node/token.yaml`.
    #[serde(default, deserialize_with = "de_opt_path")]
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
            tls_mode: TlsMode::default(),
            tls_cert_dir: None,
            tls_san_extra: Vec::new(),
            token_file: None,
            max_body_bytes: default_max_body(),
        }
    }
}

/// Agent-to-agent mesh configuration (the `swarm` section).
///
/// Controls how this node participates in the sven agent network: which port
/// it listens on for peer connections, its identity, and which other agents it
/// trusts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Listen address for the agent mesh.
    ///
    /// Other sven nodes dial this address to exchange tasks.  Use a fixed port
    /// and open it in your firewall when connecting across machines, e.g.
    /// `/ip4/0.0.0.0/tcp/4010`.
    ///
    /// Default: OS-assigned random port (`/ip4/0.0.0.0/tcp/0`).
    #[serde(default = "default_swarm_listen")]
    pub listen: String,

    /// Path for persisting the agent mesh Ed25519 keypair.
    ///
    /// When absent a new keypair is generated on every restart (ephemeral
    /// identity — peer nodes would need to re-add this node to their `peers`
    /// list after each restart).  Defaults to
    /// `~/.config/sven/node/agent-keypair`.
    #[serde(default, deserialize_with = "de_opt_path")]
    pub keypair_path: Option<PathBuf>,

    /// Identity this node advertises to peer agents on connection.
    #[serde(default)]
    pub agent: AgentIdentityConfig,

    /// Discovery rooms.  A room is a named namespace; only peers configured in
    /// the same room discover each other via mDNS or relay.
    /// Default: `["default"]`.
    #[serde(default = "default_rooms")]
    pub rooms: Vec<String>,

    /// Allowed agent peers (deny-all if empty).
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
    /// Both nodes must list each other — authorization is not automatic.
    #[serde(default)]
    pub peers: HashMap<String, String>,
}

/// Operator control node configuration (the `control` section).
///
/// **Disabled by default** — this entire section can be omitted to run without
/// a control node.  Add it only when you need to pair a native or mobile
/// operator client via `sven node authorize`.
///
/// The control node is completely separate from the agent mesh (`swarm`): it
/// carries human operator commands over the `/sven/control/1.0.0` protocol,
/// not agent-to-agent tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlConfig {
    /// Listen address for the operator control channel.
    ///
    /// Because this channel grants full control of the agent, the default
    /// binds to **loopback only** (`/ip4/127.0.0.1/tcp/0`).  To allow a
    /// mobile client on the same LAN, change this to
    /// `/ip4/0.0.0.0/tcp/4009` and open the port in your firewall.
    #[serde(default = "default_control_listen")]
    pub listen: String,

    /// Path for persisting the control node Ed25519 keypair.
    ///
    /// When absent a new keypair is generated on every restart — mobile
    /// operators would need to re-pair after each restart.
    #[serde(default, deserialize_with = "de_opt_path")]
    pub keypair_path: Option<PathBuf>,

    /// YAML file listing authorized operator peer IDs and their roles.
    ///
    /// Default: `~/.config/sven/node/authorized_peers.yaml`
    ///
    /// See [`crate::p2p::auth::PeerAllowlist`] for the file format.
    #[serde(default, deserialize_with = "de_opt_path")]
    pub authorized_peers_file: Option<PathBuf>,
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

fn default_swarm_listen() -> String {
    "/ip4/0.0.0.0/tcp/0".to_string()
}

fn default_control_listen() -> String {
    "/ip4/127.0.0.1/tcp/0".to_string()
}

fn default_rooms() -> Vec<String> {
    vec!["default".to_string()]
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            listen: default_swarm_listen(),
            keypair_path: None,
            agent: AgentIdentityConfig::default(),
            rooms: default_rooms(),
            peers: HashMap::new(),
        }
    }
}

/// Web terminal configuration (the `web` section).
///
/// Enables a browser-based terminal at `/web`. Authentication uses WebAuthn
/// passkeys — device-bound biometrics (Face ID, Touch ID, fingerprint). New
/// devices are placed in a `Pending` state until approved with
/// `sven node web-devices approve <id>`.
///
/// **WebAuthn requires HTTPS.** The `rp_id` must match the hostname or IP
/// address that the browser uses to reach this node.
///
/// # Minimal example
/// ```yaml
/// web:
///   rp_id: "192.168.1.10"
///   rp_origin: "https://192.168.1.10:18790"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    /// WebAuthn relying party ID (the "effective domain").
    ///
    /// For local network access set this to the node's IP or hostname.
    /// Default: `"localhost"` (loopback only).
    #[serde(default = "default_rp_id")]
    pub rp_id: String,

    /// Full HTTPS origin that browsers use to reach this node.
    ///
    /// Must match what appears in the browser address bar exactly.
    /// Default: `"https://localhost:18790"`.
    #[serde(default = "default_rp_origin")]
    pub rp_origin: String,

    /// Human-readable name shown to users during the WebAuthn ceremony.
    #[serde(default = "default_rp_name")]
    pub rp_name: String,

    /// Path to the YAML file storing registered web devices.
    /// Default: `~/.config/sven/node/web_devices.yaml`.
    #[serde(default, deserialize_with = "de_opt_path")]
    pub devices_file: Option<PathBuf>,

    /// Session JWT lifetime in seconds. Default: 86400 (24 hours).
    #[serde(default = "default_session_ttl")]
    pub session_ttl_secs: u64,

    /// Command to run inside the PTY session.
    ///
    /// When spawned by a running node, `SVEN_NODE_URL` and
    /// `SVEN_NODE_TOKEN` are injected into the environment automatically,
    /// so the sven TUI detects the node and connects to it for full P2P
    /// access.  Sessions persist across browser reconnects: the process keeps
    /// running while the WebSocket is temporarily closed.
    ///
    /// Default: `["sven"]` — runs the sven TUI directly (no tmux wrapper).
    #[serde(default = "default_pty_command")]
    pub pty_command: Vec<String>,
}

fn default_rp_id() -> String {
    "localhost".to_string()
}
fn default_rp_origin() -> String {
    "https://localhost:18790".to_string()
}
fn default_rp_name() -> String {
    "Sven Node".to_string()
}
fn default_session_ttl() -> u64 {
    86_400
}
fn default_pty_command() -> Vec<String> {
    // Run sven directly.  The node injects SVEN_NODE_URL and
    // SVEN_NODE_TOKEN so the TUI auto-connects to this node for full
    // P2P peer access.
    vec!["sven".to_string()]
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            rp_id: default_rp_id(),
            rp_origin: default_rp_origin(),
            rp_name: default_rp_name(),
            devices_file: None,
            session_ttl_secs: default_session_ttl(),
            pty_command: default_pty_command(),
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
    paths.push(PathBuf::from("/etc/sven/node.yaml"));
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config/sven/node.yaml"));
    }
    paths.push(PathBuf::from(".sven/node.yaml"));
    paths
}

pub fn load(extra: Option<&Path>) -> anyhow::Result<NodeConfig> {
    let mut merged = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());

    for path in config_search_paths() {
        if path.is_file() {
            debug!(path = %path.display(), "loading node config layer");
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let layer: serde_yaml::Value = serde_yaml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            merge_yaml(&mut merged, layer);
        }
    }

    if let Some(p) = extra {
        debug!(path = %p.display(), "loading explicit node config");
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let layer: serde_yaml::Value =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        merge_yaml(&mut merged, layer);
    }

    let config: NodeConfig = if matches!(&merged, serde_yaml::Value::Mapping(m) if m.is_empty()) {
        NodeConfig::default()
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
        let c = NodeConfig::default();
        assert!(
            c.http.bind.starts_with("127.0.0.1"),
            "default must be loopback-only"
        );
    }

    #[test]
    fn default_tls_is_enabled() {
        let c = NodeConfig::default();
        assert!(!c.http.insecure_dev_mode, "TLS must be on by default");
    }

    #[test]
    fn default_control_node_is_disabled() {
        let c = NodeConfig::default();
        assert!(
            c.control.is_none(),
            "control node must be disabled by default"
        );
    }

    #[test]
    fn default_swarm_peers_deny_all() {
        let c = NodeConfig::default();
        assert!(c.swarm.peers.is_empty(), "swarm must deny-all by default");
    }

    #[test]
    fn control_config_listen_defaults_to_loopback() {
        let yaml = "control:\n  keypair_path: null\n";
        let c: NodeConfig = serde_yaml::from_str(yaml).unwrap();
        let ctrl = c.control.expect("control section present");
        assert!(
            ctrl.listen.contains("127.0.0.1"),
            "control listen must default to loopback, got: {}",
            ctrl.listen
        );
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
        let c = NodeConfig::default();
        let yaml = serde_yaml::to_string(&c).unwrap();
        let back: NodeConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.http.bind, c.http.bind);
        assert_eq!(back.http.insecure_dev_mode, c.http.insecure_dev_mode);
    }

    #[test]
    fn config_insecure_dev_mode_can_be_set() {
        let yaml = "http:\n  insecure_dev_mode: true\n";
        let c: NodeConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(c.http.insecure_dev_mode);
    }

    #[test]
    fn load_returns_defaults_when_no_files_exist() {
        let c = load(None).unwrap();
        assert_eq!(c.http.bind, default_http_bind());
    }
}
