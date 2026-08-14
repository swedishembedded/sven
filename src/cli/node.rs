// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;
use std::path::PathBuf;

// ── Node subcommand ───────────────────────────────────────────────────────────

/// `sven node` subcommands.
#[derive(Subcommand, Debug)]
pub enum NodeCommands {
    /// Start the sven node (agent + HTTP + P2P).
    ///
    /// Exposes the agent over HTTPS/WebSocket and libp2p so it can be
    /// controlled from a mobile app, Slack, or any other operator client.
    ///
    /// TLS is enabled by default. A bearer token is generated on first run
    /// and printed once. Mobile/native clients can be authorized via
    /// `sven node authorize`; CLI clients use the bearer token directly.
    Start {
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,

        /// Model to use for the node's orchestrator agent.
        ///
        /// Overrides the `model.name` field from the config file.
        /// Accepts a bare model name ("claude-sonnet-4-6") or a
        /// "provider/model" pair ("anthropic/claude-sonnet-4-6").
        /// May also be set via the SVEN_MODEL environment variable.
        #[arg(long, short = 'M', env = "SVEN_MODEL", value_name = "MODEL")]
        model: Option<String>,

        /// Provider to use for the node's orchestrator agent.
        ///
        /// Overrides the `model.provider` field from the config file without
        /// changing the model name.  Use this when the model name alone is
        /// unambiguous but you want to select a different backend
        /// (e.g. "--provider openai" vs "--provider azure").
        /// When `--model` already contains a "provider/model" pair this flag
        /// is redundant.
        #[arg(long, short = 'P', value_name = "PROVIDER")]
        provider: Option<String>,

        /// Skip TLS certificate verification in web-terminal PTY sessions.
        ///
        /// When set, the node injects `SVEN_NODE_INSECURE=1` into the
        /// environment of every sven subprocess it spawns for browser web-
        /// terminal sessions.  This lets the spawned sven process connect
        /// back to the node over `wss://` without trusting the node's local-CA
        /// certificate.
        ///
        /// Use this when the node is running with its default local-CA TLS
        /// mode (not `insecure_dev_mode`) and you have not installed the CA
        /// cert into the system trust store.  Must be explicitly requested -
        /// the node never injects this flag automatically unless TLS is fully
        /// disabled via `insecure_dev_mode`.
        #[arg(long, default_value_t = false)]
        pty_insecure: bool,
    },

    /// Authorize a mobile/native operator device to control this node via P2P.
    ///
    /// This is for native clients (e.g. a mobile app) that connect over libp2p
    /// rather than HTTP.  For CLI use, the bearer token (`sven node exec`) is
    /// the simpler path and does not require this command.
    ///
    /// The operator device displays a `sven://` URI (or QR code).
    /// Paste it here; the peer ID and fingerprint are shown for confirmation
    /// before any change is written to disk.
    ///
    /// Note: this has nothing to do with connecting two sven nodes together.
    /// Node-to-node connections happen automatically via mDNS or relay.
    Authorize {
        /// The `sven://` URI displayed by the operator device.
        uri: String,
        /// Human-readable label for this device (e.g. "my-phone").
        #[arg(long, short = 'l')]
        label: Option<String>,
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Revoke a previously authorized operator device.
    Revoke {
        /// PeerId (base58) to revoke.
        peer_id: String,
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Regenerate the HTTP bearer token.
    ///
    /// The new token is printed once. The old token is immediately invalidated.
    RegenerateToken {
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Print the current node configuration and exit.
    ShowConfig {
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// List all authorized operator devices.
    ///
    /// Shows the devices in `authorized_peers.yaml` - the human operator
    /// devices (phones, laptops, CLI clients) authorized to control this
    /// node via P2P.  Devices are added by redeeming the pairing QR/URI the
    /// node prints at startup (`sven connect <uri>` or the mobile app), or
    /// with `sven node authorize`; remove them with `sven node revoke`.
    ///
    /// Note: this is NOT the same as the agent `list_peers` tool, which
    /// shows other sven nodes available for task delegation.
    ListOperators {
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Send a task to a running node and stream the response.
    ///
    /// Connects to the local node over WebSocket and submits a task as
    /// if you were using the web UI.  The response is streamed to stdout.
    ///
    /// The bearer token must be provided via the SVEN_NODE_TOKEN
    /// environment variable (or the legacy SVEN_GATEWAY_TOKEN) or --token.
    ///
    /// Example:
    ///   export SVEN_NODE_TOKEN=<token shown at first startup>
    ///   sven node exec "delegate a task to say hi to agent local"
    Exec {
        /// The task to send to the agent.
        task: String,
        /// Bearer token (or set SVEN_NODE_TOKEN / SVEN_GATEWAY_TOKEN).
        #[arg(long, env = "SVEN_NODE_TOKEN")]
        token: String,
        /// Node WebSocket URL.
        #[arg(long, default_value = "wss://127.0.0.1:18790/ws")]
        url: String,
        /// Path to the node config file (used to locate the TLS cert).
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Skip TLS certificate verification (unsafe - for dev only).
        #[arg(long)]
        insecure: bool,
    },

    /// Manage browser devices registered for the web terminal.
    ///
    /// New browser devices start in `pending` state and must be approved
    /// before they can open a terminal session.  This command connects to
    /// the running node (via the bearer token) to approve/revoke devices
    /// without restarting.
    ///
    /// Example workflow:
    ///   1. Mobile browser visits https://node-ip:18790/web and registers a passkey.
    ///   2. The device ID is shown on screen ("awaiting approval").
    ///   3. Admin runs: sven node web-devices approve <device-id>
    ///   4. Browser immediately transitions to the terminal.
    WebDevices {
        #[command(subcommand)]
        command: WebDevicesCommands,
    },

    /// Print the local CA certificate and platform-specific trust instructions.
    ///
    /// When `tls_mode` is `local-ca` or `auto` (default), sven generates a
    /// local CA certificate on first start.  Run this command once on each
    /// device that should trust the node - it will print the exact commands
    /// needed for your platform (macOS, Linux, iOS, Android).
    ///
    /// Example:
    ///   sven node install-ca
    ///   sven node install-ca --config /etc/sven/node.yaml
    InstallCa {
        /// Path to the node config file (locates the TLS cert directory).
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Print the local CA certificate PEM to stdout.
    ///
    /// Useful for piping to other tools, serving over HTTP for mobile import,
    /// or adding to a custom trust bundle:
    ///
    ///   sven node export-ca > ca.pem
    ///   python3 -m http.server --directory . 8080  # then open on phone
    ExportCa {
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },
}


/// `sven node web-devices` subcommands.
#[derive(Subcommand, Debug)]
pub enum WebDevicesCommands {
    /// List registered browser devices.
    List {
        /// Filter by status: pending, approved, revoked, or all (default).
        #[arg(long, default_value = "all")]
        filter: String,
        /// Bearer token (or set SVEN_NODE_TOKEN / SVEN_GATEWAY_TOKEN).
        #[arg(long, env = "SVEN_NODE_TOKEN")]
        token: String,
        /// Node WebSocket URL.
        #[arg(long, default_value = "wss://127.0.0.1:18790/ws")]
        url: String,
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Skip TLS certificate verification (unsafe - for dev only).
        #[arg(long)]
        insecure: bool,
    },

    /// Approve a pending browser device.
    ///
    /// The device UUID (or a unique prefix) is shown in the browser's
    /// "awaiting approval" screen.  This command sends the approval to the
    /// running node immediately - no restart required.
    Approve {
        /// Full device UUID or unique prefix (e.g. "abc1234" matches "abc1234ef-...").
        device_id: String,
        /// Bearer token (or set SVEN_NODE_TOKEN / SVEN_GATEWAY_TOKEN).
        #[arg(long, env = "SVEN_NODE_TOKEN")]
        token: String,
        /// Node WebSocket URL.
        #[arg(long, default_value = "wss://127.0.0.1:18790/ws")]
        url: String,
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Skip TLS certificate verification (unsafe - for dev only).
        #[arg(long)]
        insecure: bool,
    },

    /// Revoke an approved browser device.
    ///
    /// The device is immediately blocked; any open PTY session is terminated.
    Revoke {
        /// Full device UUID or unique prefix.
        device_id: String,
        /// Bearer token (or set SVEN_NODE_TOKEN / SVEN_GATEWAY_TOKEN).
        #[arg(long, env = "SVEN_NODE_TOKEN")]
        token: String,
        /// Node WebSocket URL.
        #[arg(long, default_value = "wss://127.0.0.1:18790/ws")]
        url: String,
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Skip TLS certificate verification (unsafe - for dev only).
        #[arg(long)]
        insecure: bool,
    },
}

