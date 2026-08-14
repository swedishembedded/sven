// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

// ── Cloud subcommand ──────────────────────────────────────────────────────────

/// Role a minted cloud token acts under (mirrors `sven_cloud::Role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CloudRoleArg {
    /// Customer-side companion: may attach to the tether, nothing else.
    Companion,
    /// Full control of a tenant: manage users, mint/revoke tokens, run sessions.
    Operator,
    /// Read-only visibility for the tenant's customer.
    ClientViewer,
}

/// How `sven cloud serve` terminates TLS on the tether endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum CloudTlsArg {
    /// Generate an in-memory CA + loopback certificate at startup (the CA PEM
    /// is written out so companions can trust it). Safe local-dev default.
    #[default]
    LocalCa,
    /// Alias of `local-ca` (same in-memory self-signed material).
    SelfSigned,
    /// Terminate TLS with PEM `--cert` / `--key` files (production).
    Files,
    /// DANGER: plaintext `ws://` — bearer tokens cross the wire in cleartext.
    /// Local testing only.
    InsecureDev,
}

/// `sven cloud` subcommands - operate the managed-agents control plane.
///
/// The tenant/token/session admin commands operate directly on the SQLite
/// control-plane database (shared with `sven cloud serve`); `serve` runs the
/// long-lived tether endpoint companions dial out to.
///
/// Quick start:
///
///   sven cloud tenant create Acme
///   sven cloud token mint --tenant acme --role companion
///   sven cloud serve                       # long-running; prints tether URL + CA
#[derive(Subcommand, Debug)]
pub enum CloudCommands {
    /// Start the control plane: the WSS tether endpoint companions dial out to.
    ///
    /// Authenticates companions against the tokens minted with
    /// `sven cloud token mint` (store-backed, expiring, revocable). TLS is on
    /// by default (`local-ca`): an in-memory CA + loopback certificate is
    /// generated at startup and the CA PEM written to `--ca-out` so companions
    /// can trust it. Runs until Ctrl-C.
    Serve {
        /// SQLite control-plane database path.
        #[arg(long, env = "SVEN_CLOUD_DB", default_value = "sven-cloud.db")]
        db: PathBuf,
        /// Bind address (port 0 picks a free port).
        #[arg(long, env = "SVEN_CLOUD_BIND", default_value = "127.0.0.1:8443")]
        bind: String,
        /// TLS mode: local-ca (default), self-signed, files, or insecure-dev.
        #[arg(long, value_enum, default_value_t = CloudTlsArg::LocalCa)]
        tls: CloudTlsArg,
        /// Server certificate chain PEM (required for `--tls files`).
        #[arg(long, value_name = "PEM", required_if_eq("tls", "files"))]
        cert: Option<PathBuf>,
        /// Private key PEM (required for `--tls files`).
        #[arg(long, value_name = "PEM", required_if_eq("tls", "files"))]
        key: Option<PathBuf>,
        /// Where to write the generated CA PEM (local-ca/self-signed only).
        /// Defaults to `cloud-ca.pem` beside the database.
        #[arg(long, value_name = "PEM")]
        ca_out: Option<PathBuf>,
        /// libp2p multiaddr the embedded P2P **relay** listens on, so NAT'd
        /// `sven node`s can reserve a circuit here and be paired/reached across
        /// NAT. Set to `off` to disable the relay. The relay keypair is
        /// persisted beside the database, so its PeerId is stable.
        #[arg(
            long,
            env = "SVEN_CLOUD_RELAY_LISTEN",
            default_value = "/ip4/0.0.0.0/tcp/4002",
            value_name = "MULTIADDR|off"
        )]
        relay_listen: String,
        /// Telegram bot token enabling consultant steering of shared sessions
        /// from Telegram. Omit to leave the feature off (no regression).
        /// Requires `--telegram-operator-token`.
        #[arg(long, env = "SVEN_TELEGRAM_BOT_TOKEN", value_name = "TOKEN")]
        telegram_bot_token: Option<String>,
        /// Comma-separated Telegram user IDs allowed to steer via the bot.
        /// Required when `--telegram-bot-token` is set, unless every bot
        /// user is explicitly admitted with `--telegram-allow-all`.
        #[arg(
            long,
            env = "SVEN_TELEGRAM_ALLOWED_USERS",
            value_name = "IDS",
            value_delimiter = ','
        )]
        telegram_allowed_users: Vec<i64>,
        /// Explicitly allow EVERY authenticated bot user to steer shared
        /// sessions (not recommended). Without this flag an empty
        /// `--telegram-allowed-users` list refuses to start rather than
        /// silently admitting everyone.
        #[arg(long, env = "SVEN_TELEGRAM_ALLOW_ALL")]
        telegram_allow_all: bool,
        /// Operator bearer token the Telegram bridge acts as (its tenant/role
        /// scope every steer). The identity is from THIS token, never the chat.
        #[arg(long, env = "SVEN_TELEGRAM_OPERATOR_TOKEN", value_name = "TOKEN")]
        telegram_operator_token: Option<String>,
    },

    /// Manage tenants (paying customers / accounts).
    Tenant {
        #[command(subcommand)]
        command: CloudTenantCommands,
    },

    /// Mint and revoke per-tenant bearer tokens.
    Token {
        #[command(subcommand)]
        command: CloudTokenCommands,
    },

    /// Open a gated cloud agent session.
    Session {
        #[command(subcommand)]
        command: CloudSessionCommands,
    },

    /// Drive a companion-backed cloud session from the normal sven UI.
    ///
    /// The one-command way to operate a cloud session over the interactive
    /// operator endpoint (`GET /operator/ws`) — the same WebSocket transport the
    /// normal `sven` TUI uses as its node backend.
    ///
    /// With no `--prompt` this launches the FULL interactive TUI wired to that
    /// operator endpoint, so the operator gets the ordinary sven UI driving the
    /// cloud/companion session (equivalent to setting `SVEN_NODE_URL` +
    /// `SVEN_NODE_TOKEN` and running `sven`).
    ///
    /// With `--prompt` it runs a NON-interactive, scriptable one-shot: connect,
    /// open a session, send the prompt, stream the events to stdout, exit.
    ///
    ///   sven cloud connect --url https://cloud.example.com --token "$OP"
    ///   sven cloud connect --url https://cloud.example.com --token "$OP" \
    ///     --prompt "read the report"
    Connect {
        /// Control-plane base URL, e.g. `https://cloud.example.com`. The
        /// operator WS URL is derived from it (`https`→`wss`, append
        /// `/operator/ws`).
        #[arg(long, env = "SVEN_CLOUD_URL", default_value = "https://localhost:8443")]
        url: String,
        /// Operator bearer token (mint with
        /// `sven cloud token mint --role operator`). Falls back to
        /// `SVEN_NODE_TOKEN` when `--token`/`SVEN_CLOUD_TOKEN` are unset.
        #[arg(long, env = "SVEN_CLOUD_TOKEN")]
        token: Option<String>,
        /// Agent mode to run as.
        #[arg(long, default_value = "agent")]
        mode: String,
        /// Optional one-shot prompt. When set, runs non-interactively and exits
        /// once the session finishes; when omitted, opens the interactive TUI.
        #[arg(long)]
        prompt: Option<String>,
        /// Extra CA certificate PEM to trust (for a self-signed control plane;
        /// the file `sven cloud serve` writes as `cloud-ca.pem`).
        #[arg(long, value_name = "PEM")]
        ca_cert: Option<PathBuf>,
        /// Disable TLS verification — local testing only.
        #[arg(long)]
        insecure: bool,
    },

    /// Provision a ready-to-drive demo tenant in one shot (turnkey quickstart).
    ///
    /// Creates the tenant (idempotent), books the current-period platform fee
    /// so the subscription is active, grants demo credit so the balance is
    /// positive, and mints BOTH an operator token and a companion token. The
    /// two secrets are written to `operator.token` / `companion.token` in the
    /// output directory (0600) and the exact `sven-companion` and `sven cloud
    /// session start` commands are printed.
    ///
    /// Point `--db` at the same database `sven cloud serve` uses (the credit
    /// ledger and CA cert live beside it); everything the [`SessionGate`]
    /// checks is satisfied against that shared state. Intended for local/demo
    /// use — for production, grant credit and mint tokens deliberately.
    DemoSeed {
        /// Human-readable tenant name (its id is a slug of this).
        #[arg(long, default_value = "Demo Inc")]
        tenant_name: String,
        /// SQLite control-plane database path (shared with `sven cloud serve`).
        #[arg(long, env = "SVEN_CLOUD_DB", default_value = "sven-cloud.db")]
        db: PathBuf,
        /// Portal base URL printed in the operator command.
        #[arg(long, env = "SVEN_CLOUD_URL", default_value = "https://localhost:8443")]
        url: String,
        /// Demo credit to grant, in micro-USD (default: $100).
        #[arg(long, default_value_t = 100_000_000)]
        credit_micro_usd: i64,
        /// Current-period platform fee to book, in micro-USD (default: $1).
        #[arg(long, default_value_t = 1_000_000)]
        fee_micro_usd: i64,
        /// Token lifetime (e.g. "30d", "12h"); clamped to each role's cap.
        #[arg(long, value_name = "DURATION", default_value = "30d")]
        ttl: String,
        /// Directory to write the token files to (default: beside `--db`).
        #[arg(long, value_name = "DIR")]
        out_dir: Option<PathBuf>,
    },
}

/// `sven cloud tenant` subcommands.
#[derive(Subcommand, Debug)]
pub enum CloudTenantCommands {
    /// Create a tenant; prints its id (a slug of the name).
    Create {
        /// Human-readable tenant name (e.g. "Acme Inc").
        name: String,
        /// Optional subscription plan label (informational).
        #[arg(long)]
        plan: Option<String>,
        /// SQLite control-plane database path.
        #[arg(long, env = "SVEN_CLOUD_DB", default_value = "sven-cloud.db")]
        db: PathBuf,
    },
    /// List all tenants.
    List {
        /// SQLite control-plane database path.
        #[arg(long, env = "SVEN_CLOUD_DB", default_value = "sven-cloud.db")]
        db: PathBuf,
    },
}

/// `sven cloud token` subcommands.
#[derive(Subcommand, Debug)]
pub enum CloudTokenCommands {
    /// Mint a token; the raw secret is printed to stdout exactly ONCE.
    ///
    /// Metadata (id, role, expiry) goes to stderr; the secret alone goes to
    /// stdout so it can be captured (`sven cloud token mint ... > token.txt`).
    /// It is never logged and cannot be recovered afterwards.
    Mint {
        /// Tenant id the token is scoped to.
        #[arg(long)]
        tenant: String,
        /// Role the token grants.
        #[arg(long, value_enum)]
        role: CloudRoleArg,
        /// Lifetime (e.g. "30d", "12h"). Defaults to the role's maximum; the
        /// server clamps any request to the per-role cap.
        #[arg(long, value_name = "DURATION")]
        ttl: Option<String>,
        /// SQLite control-plane database path.
        #[arg(long, env = "SVEN_CLOUD_DB", default_value = "sven-cloud.db")]
        db: PathBuf,
    },
    /// Revoke a token by id (idempotent).
    Revoke {
        /// Token id (as printed by `mint`).
        token_id: String,
        /// SQLite control-plane database path.
        #[arg(long, env = "SVEN_CLOUD_DB", default_value = "sven-cloud.db")]
        db: PathBuf,
    },
}

/// `sven cloud session` subcommands.
#[derive(Subcommand, Debug)]
pub enum CloudSessionCommands {
    /// Start and stream a cloud agent session against a running `sven cloud
    /// serve`, as an OPERATOR.
    ///
    /// Authenticates to the control plane with an operator token, `POST`s
    /// `/sessions` with the prompt (which the server gates on subscription +
    /// credit, then drives a real kernel: metered LLM in the cloud, tool calls
    /// routed to the tenant's companion), and streams the live event feed —
    /// output, tool activity and approvals — until the session finishes.
    Start {
        /// Portal base URL of the running control plane, e.g.
        /// `https://cloud.example.com` (no trailing path).
        #[arg(long, env = "SVEN_CLOUD_URL", default_value = "https://localhost:8443")]
        url: String,
        /// Operator bearer token (mint with
        /// `sven cloud token mint --role operator`).
        #[arg(long, env = "SVEN_CLOUD_TOKEN")]
        token: String,
        /// Initial prompt for the session.
        #[arg(long)]
        prompt: String,
        /// Agent mode to run as.
        #[arg(long, default_value = "agent")]
        mode: String,
        /// Extra CA certificate PEM to trust (for a self-signed control plane;
        /// the file `sven cloud serve` writes as `cloud-ca.pem`).
        #[arg(long, value_name = "PEM")]
        ca_cert: Option<PathBuf>,
        /// Disable TLS verification — local testing only.
        #[arg(long)]
        insecure: bool,
    },

    /// Attach to a SHARED local session and steer it, as an OPERATOR/consultant.
    ///
    /// The inverse of `start`: here the session's brain runs on the customer's
    /// machine (started with `sven share`) and this command drives it remotely.
    /// Authenticates with an operator token, dials `GET /share/:share_id`,
    /// creates a session on the shared kernel, sends `--prompt` down, and
    /// streams the reply until the turn finishes.
    Attach {
        /// Portal base URL of the running control plane, e.g.
        /// `https://cloud.example.com` (no trailing path).
        #[arg(long, env = "SVEN_CLOUD_URL", default_value = "https://localhost:8443")]
        url: String,
        /// The shared session's id (printed by `sven share`).
        #[arg(long)]
        share_id: String,
        /// Operator bearer token (mint with
        /// `sven cloud token mint --role operator`).
        #[arg(long, env = "SVEN_CLOUD_TOKEN")]
        token: String,
        /// One-shot prompt to steer the shared session with. Omit to attach and
        /// stream read-only.
        #[arg(long)]
        prompt: Option<String>,
        /// Agent mode to run as.
        #[arg(long, default_value = "agent")]
        mode: String,
        /// Extra CA certificate PEM to trust (for a self-signed control plane).
        #[arg(long, value_name = "PEM")]
        ca_cert: Option<PathBuf>,
        /// Disable TLS verification — local testing only.
        #[arg(long)]
        insecure: bool,
    },
}
