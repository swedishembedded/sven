// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use std::path::PathBuf;
use sven_config::AgentMode;

// ── Tool subcommand ───────────────────────────────────────────────────────────

/// `sven tool` subcommands.
///
/// Run individual built-in tools directly from the command line — useful for
/// scripting, debugging tool behaviour, or quick one-off operations without
/// starting an agent session.
///
/// Examples:
///
///   sven tool list
///   sven tool call read_file path=src/main.rs
///   sven tool call grep pattern=TODO path=./src include="*.rs"
///   sven tool call shell command="git status"
///   sven tool call grep --help
#[derive(Subcommand, Debug)]
pub enum ToolCommands {
    /// List all available built-in tools with their names and descriptions.
    ///
    /// Example:
    ///
    ///   sven tool list
    List,

    /// Call a built-in tool directly.
    ///
    /// With no arguments, or with only `--help`, prints all tools and their
    /// complete parameter schemas.
    ///
    /// With a tool name as the first argument and no further arguments, prints
    /// that tool's parameter schema.  Add key=value pairs to execute the tool.
    ///
    /// Parameter forms:
    ///   key=value            — string, bool (true/false), or integer
    ///   --json '{"k":"v"}'   — raw JSON object (overrides key=value pairs)
    ///
    /// Examples:
    ///
    ///   sven tool call                                 — list all tools + schemas
    ///   sven tool call --help                          — same
    ///   sven tool call grep                            — show grep's schema
    ///   sven tool call grep --help                     — show grep's schema
    ///   sven tool call grep pattern=TODO path=./src
    ///   sven tool call shell command="git status"
    ///   sven tool call write_file path=/tmp/out.txt content="hello"
    ///   sven tool call run_terminal_command --json '{"command":"ls -la"}'
    // disable_help_flag so --help lands in `args` and we can show tool-specific docs
    #[command(disable_help_flag = true)]
    Call {
        /// Everything after `call`:
        ///   (no args)              → print all tools + schemas
        ///   --help / -h            → same
        ///   <TOOL>                 → print that tool's schema
        ///   <TOOL> --help          → same
        ///   <TOOL> key=value ...   → execute the tool
        ///   <TOOL> --json '{...}'  → execute with raw JSON args
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

// ── Mcp subcommand ────────────────────────────────────────────────────────────

/// `sven mcp` subcommands.
#[derive(Subcommand, Debug)]
pub enum McpCommands {
    /// Expose sven as an MCP server over stdio.
    ///
    /// Starts a Model Context Protocol server that speaks line-delimited
    /// JSON-RPC on stdin/stdout.  Any MCP-compatible host can launch sven
    /// as a subprocess and call its tools:
    ///
    ///   Cursor / Claude Desktop / opencode (`mcp.json`):
    ///
    ///   { "mcpServers": { "sven": { "command": "sven", "args": ["mcp", "serve"] } } }
    ///
    /// The server blocks until stdin reaches EOF (i.e. until the host
    /// disconnects).  It does not fork, does not bind a port, and requires
    /// no authentication — security is inherited from the host process.
    Serve {
        /// Comma-separated list of tool names to expose (local mode only).
        ///
        /// Defaults to all MCP-safe built-in tools (see `sven_mcp::DEFAULT_TOOL_NAMES`).
        /// Pass `all` to include every registered tool explicitly.
        /// Ignored when `--node-url` is set (the node's own registry is used).
        ///
        /// Example: --tools read_file,write_file,grep,run_terminal_command
        #[arg(long, value_name = "TOOL,...")]
        tools: Option<String>,

        /// Brave Search API key for the web_search tool (local mode only).
        ///
        /// May also be provided via the BRAVE_API_KEY environment variable.
        /// Ignored when `--node-url` is set.
        #[arg(long, env = "BRAVE_API_KEY", value_name = "KEY")]
        brave_api_key: Option<String>,

        /// WebSocket URL of a running `sven node` to proxy tool calls through.
        ///
        /// When provided, the MCP server connects to the node over WebSocket
        /// and forwards every tool call to it.  This exposes the full node tool
        /// registry, including P2P tools like `list_peers` and `delegate_task`.
        ///
        /// Example: --node-url wss://127.0.0.1:18790/ws
        #[arg(long, value_name = "URL")]
        node_url: Option<String>,

        /// Bearer token for authenticating with the sven node.
        ///
        /// Required when `--node-url` is set.  This is the raw token printed by
        /// `sven node start` on first launch (not the hash stored on disk).
        ///
        /// May also be provided via the SVEN_NODE_TOKEN environment variable.
        /// The legacy name SVEN_GATEWAY_TOKEN is also accepted.
        #[arg(long, env = "SVEN_NODE_TOKEN", value_name = "TOKEN")]
        token: Option<String>,
    },
}

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
        /// cert into the system trust store.  Must be explicitly requested —
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
    /// Shows the devices in `authorized_peers.yaml` — the human operator
    /// devices (phones, laptops, CLI clients) authorized to control this
    /// node via P2P.  Use `sven node pair` to add devices and
    /// `sven node revoke` to remove them.
    ///
    /// Note: this is NOT the same as the agent `list_peers` tool, which
    /// shows other sven nodes available for task delegation.
    /// Use `sven node authorize` to add devices, `sven node revoke` to remove.
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
        /// Skip TLS certificate verification (unsafe — for dev only).
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
    /// device that should trust the node — it will print the exact commands
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

// ── Peer subcommand ───────────────────────────────────────────────────────────

/// `sven peer` subcommands.
///
/// All commands start an ephemeral P2P node using the same persistent keypair
/// as `sven node start` — no separate daemon is needed.
#[derive(Subcommand, Debug)]
pub enum PeerCommands {
    /// List all agent peers discovered on the network.
    ///
    /// Starts an ephemeral P2P node, waits for mDNS and relay discovery, then
    /// prints every peer that is connected and has announced itself.
    ///
    /// Example:
    ///   sven peer list
    ///   sven peer list --timeout 5
    List {
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// Seconds to wait for peer discovery (default: 3).
        #[arg(long, default_value = "3")]
        timeout: u64,
    },

    /// Open an interactive chat session with a peer agent.
    ///
    /// Starts an ephemeral P2P node, connects to the named peer, and opens an
    /// interactive line-by-line conversation. Every message you type is sent as
    /// a session message; the peer's reply is shown when it arrives.
    ///
    /// Recent conversation history (since the last 1-hour break) is shown at
    /// the start of the session.
    ///
    /// Examples:
    ///   sven peer chat backend-agent
    ///   sven peer chat 12D3KooWAbCdEfGh…
    Chat {
        /// Peer agent name or base58 peer ID (prefix also works).
        peer: String,
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Grep-style regex search over local conversation history.
    ///
    /// Searches the JSONL conversation store in
    /// `~/.config/sven/conversations/peers/`.  No network connection needed.
    ///
    /// Pattern syntax: full Rust regex (same as ripgrep).
    ///   (?i)     — case-insensitive
    ///   ^ERROR   — lines starting with ERROR
    ///   \d{4}    — four consecutive digits
    ///
    /// Examples:
    ///   sven peer search "auth" --peer backend-agent
    ///   sven peer search "(?i)out.of.memory" --peer backend-agent
    ///   sven peer search "^ERROR"              (searches all peers)
    Search {
        /// Regex pattern (grep-style).
        pattern: String,
        /// Scope search to this peer agent name or peer ID.
        /// Omit to search across all peers.
        #[arg(long, short = 'p')]
        peer: Option<String>,
        /// Maximum number of results to show (default: 40).
        #[arg(long, default_value = "40")]
        limit: usize,
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
        /// Skip TLS certificate verification (unsafe — for dev only).
        #[arg(long)]
        insecure: bool,
    },

    /// Approve a pending browser device.
    ///
    /// The device UUID (or a unique prefix) is shown in the browser's
    /// "awaiting approval" screen.  This command sends the approval to the
    /// running node immediately — no restart required.
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
        /// Skip TLS certificate verification (unsafe — for dev only).
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
        /// Skip TLS certificate verification (unsafe — for dev only).
        #[arg(long)]
        insecure: bool,
    },
}

/// Output format for headless / CI runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormatArg {
    /// Full conversation format (## User / ## Sven / ## Tool / ## Tool Result).
    /// Output is valid sven conversation markdown and fully pipeable.
    #[default]
    Conversation,
    /// Structured JSON: title + array of steps with metadata.
    /// Not designed for piping between sven instances; use --output-format jsonl for that.
    Json,
    /// Compact plain text: only the final agent response for each step.
    /// Matches the legacy pre-enhancement behaviour.
    Compact,
    /// Full-fidelity JSONL: one JSON record per line (messages, thinking, tool calls).
    /// Designed for piping between sven instances:
    ///   sven 'task 1' --output-format jsonl | sven 'task 2'
    /// The receiving sven instance automatically detects and loads the history.
    Jsonl,
}

#[derive(Parser, Debug)]
#[command(
    name = "sven",
    about = "An efficient AI coding agent for CLI and CI",
    version,
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Optional initial prompt or task description
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Run headless (no TUI); outputs clean text to stdout
    #[arg(long, short = 'H')]
    pub headless: bool,

    /// Agent mode
    #[arg(long, short = 'm', value_enum, default_value = "agent")]
    pub mode: AgentMode,

    /// Model to use, e.g. "gpt-4o" or "anthropic/claude-opus-4-5"
    #[arg(long, short = 'M', env = "SVEN_MODEL")]
    pub model: Option<String>,

    /// Path to a markdown file to use as input (CI mode)
    #[arg(long, short = 'f')]
    pub file: Option<PathBuf>,

    /// Resume a saved conversation.
    /// Supply an ID (or unique prefix / file path) to resume directly.
    /// Omit the ID to pick interactively with fzf.
    /// In headless mode an explicit ID is required.
    /// Use 'sven chats' to list available conversations.
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// Path to config file (overrides auto-discovery)
    #[arg(long, short = 'c')]
    pub config: Option<PathBuf>,

    /// Enable embedded Neovim chat view (default: plain ratatui).
    #[arg(long, alias = "no-nvim")]
    pub nvim: bool,

    /// Output format for headless runs (conversation | json | compact)
    #[arg(long, value_enum, default_value = "conversation")]
    pub output_format: OutputFormatArg,

    /// Directory to write run artifacts (full conversation, per-step files).
    /// Created if it does not exist.
    #[arg(long)]
    pub artifacts_dir: Option<PathBuf>,

    /// Template variable in KEY=VALUE form, substituted as {{KEY}} in workflow steps.
    /// May be repeated: --var branch=main --var pr=42
    #[arg(long = "var", value_name = "KEY=VALUE")]
    pub vars: Vec<String>,

    /// Per-step timeout in seconds (0 = no limit). Overrides config and frontmatter.
    #[arg(long, value_name = "SECS")]
    pub step_timeout: Option<u64>,

    /// Total run timeout in seconds (0 = no limit). Overrides config and frontmatter.
    #[arg(long, value_name = "SECS")]
    pub run_timeout: Option<u64>,

    /// Parse and validate the workflow file, then exit without calling the model.
    #[arg(long)]
    pub dry_run: bool,

    /// Override the system prompt by reading from a file.
    /// The file contents are used verbatim instead of the built-in prompt.
    /// Compatible with --append-system-prompt (appended after file content).
    #[arg(long, value_name = "PATH")]
    pub system_prompt_file: Option<PathBuf>,

    /// Append text to the default system prompt (after the Guidelines section).
    /// Ignored when --system-prompt-file is given (unless both are set, in
    /// which case the text is appended after the file content).
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Option<String>,

    /// Write the final agent response to a file after the run completes.
    /// The file is created (and intermediate directories) if needed.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output_last_message: Option<PathBuf>,

    /// Load conversation history from a JSONL file before running.
    /// The file is parsed as a full-fidelity JSONL conversation; the history
    /// seeds the agent and any workflow steps run on top of it.
    /// Cannot be combined with --jsonl.
    #[arg(long, value_name = "PATH", conflicts_with = "jsonl")]
    pub load_jsonl: Option<PathBuf>,

    /// Write the output JSONL to this path after the run.
    /// If omitted, output goes to the auto-log path (.sven/logs/<timestamp>.jsonl).
    /// Cannot be combined with --jsonl.
    #[arg(long, value_name = "PATH", conflicts_with = "jsonl")]
    pub output_jsonl: Option<PathBuf>,

    /// Combined load + output JSONL: equivalent to --load-jsonl PATH --output-jsonl PATH.
    /// Loads an existing conversation from PATH, runs, and writes back to the same file.
    /// In TUI mode the file is kept in sync after every turn.
    /// If the file does not exist it is created automatically.
    #[arg(long, value_name = "PATH")]
    pub jsonl: Option<PathBuf>,

    /// Replay all tool calls in the loaded JSONL conversation with fresh results
    /// before submitting to the model.  Requires --load-jsonl or --jsonl.
    #[arg(long)]
    pub rerun_toolcalls: bool,

    /// When loading a conversation with --load-jsonl or --jsonl, regenerate the
    /// system prompt from the current skills and config instead of reusing the
    /// one stored in the JSONL file.  By default the stored system prompt is
    /// used so that resumed conversations are fully reproducible.
    #[arg(long)]
    pub regen_system_prompt: bool,

    /// Increase verbosity (-v = debug, -vv = trace)
    #[arg(long, short = 'v', action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Inspect and call built-in tools directly.
    ///
    /// Useful for scripting, debugging tool behaviour, or quick one-off
    /// operations without starting an agent session.
    ///
    ///   sven tool list                     — list all tools
    ///   sven tool call grep --help         — show grep's parameter schema
    ///   sven tool call read_file path=src/main.rs
    Tool {
        #[command(subcommand)]
        command: ToolCommands,
    },

    /// Expose sven as an MCP server for use with Cursor, Claude Desktop, and
    /// other MCP-compatible hosts.
    ///
    /// Run `sven mcp serve` to start the server.  The process blocks on
    /// stdin/stdout until the host disconnects.
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },

    /// Node: start the agent, pair devices, manage tokens.
    ///
    /// Run `sven node start` to expose this agent to mobile apps, Slack,
    /// and other clients. Run `sven node pair <uri>` to authorize a device.
    Node {
        #[command(subcommand)]
        command: NodeCommands,
    },

    /// Peer: list agents, chat, and search conversation history.
    ///
    /// Starts an ephemeral P2P connection — no running node required.
    ///
    ///   sven peer list                              — discover connected peers
    ///   sven peer chat backend-agent                — interactive chat session
    ///   sven peer search backend-agent "auth"       — grep conversation history
    ///   sven peer search --all "(?i)out.of.memory"  — search across all peers
    Peer {
        #[command(subcommand)]
        command: PeerCommands,
    },

    /// Generate shell completion script
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Print the effective configuration and exit
    ShowConfig,
    /// List saved conversations
    Chats {
        /// Maximum number of conversations to show (default: 20)
        #[arg(long, short = 'n', default_value = "20")]
        limit: usize,
    },
    /// Validate a workflow file: parse frontmatter, count steps, check syntax.
    /// Exits 0 if valid, non-zero with an error description otherwise.
    Validate {
        /// Path to the workflow markdown file to validate
        #[arg(long, short = 'f', required = true)]
        file: PathBuf,
    },
    /// List available models for the configured provider(s).
    ///
    /// By default the static built-in catalog is shown.
    /// With --refresh the configured provider API is queried for live data.
    ListModels {
        /// Filter by provider name (e.g. "openai", "anthropic", "groq")
        #[arg(long, short = 'p')]
        provider: Option<String>,
        /// Query the provider API for the live list of available models
        #[arg(long)]
        refresh: bool,
        /// Output as JSON instead of a formatted table
        #[arg(long)]
        json: bool,
    },

    /// List all supported model providers.
    ///
    /// Shows each provider's id, name, description, and default API key
    /// environment variable.  Use the provider id in your config file under
    /// `model.provider`.
    ListProviders {
        /// Show detailed information for each provider
        #[arg(long, short = 'v')]
        verbose: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

impl Cli {
    /// Returns true if the run should be headless (CI mode).
    ///
    /// Headless is triggered by any of:
    /// - `--headless` flag
    /// - stdin is not a terminal (piped input, e.g. `echo "task" | sven`)
    /// - stdout is not a terminal (piped output, e.g. `sven 'hi' | sven 'follow up'`)
    ///
    /// Checking stdout matters for the pipe case: the left side of a pipe has
    /// a TTY stdin but a piped stdout.  Without this check it would try to start
    /// the full TUI and write escape codes into the pipe, causing it to hang.
    pub fn is_headless(&self) -> bool {
        self.headless || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal()
    }

    /// Resolve the effective JSONL input path: --load-jsonl takes priority, then --jsonl.
    pub fn effective_load_jsonl(&self) -> Option<&PathBuf> {
        self.load_jsonl.as_ref().or(self.jsonl.as_ref())
    }

    /// Resolve the effective JSONL output path: --output-jsonl takes priority, then --jsonl.
    pub fn effective_output_jsonl(&self) -> Option<&PathBuf> {
        self.output_jsonl.as_ref().or(self.jsonl.as_ref())
    }
}

pub fn print_completions(shell: Shell) {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "sven", &mut std::io::stdout());
}

// TTY detection for stdin and stdout.
trait IsTerminal {
    fn is_terminal(&self) -> bool;
}

impl IsTerminal for std::io::Stdin {
    fn is_terminal(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe { libc::isatty(self.as_raw_fd()) != 0 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

impl IsTerminal for std::io::Stdout {
    fn is_terminal(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe { libc::isatty(self.as_raw_fd()) != 0 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}
