// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven` CLI surface: top-level [`Cli`] args, the [`Commands`] dispatch
//! enum, and one module per subcommand group's own nested `*Commands` enum.
//!
//! This mirrors `src/run/` (see `src/main.rs`), which holds the *handler*
//! for each of these groups — this module only declares the clap grammar.

mod acp;
mod cloud;
mod index;
mod mcp;
mod node;
mod peer;
mod team;
mod tool;

pub use acp::AcpCommands;
pub use cloud::{
    CloudCommands, CloudRoleArg, CloudSessionCommands, CloudTenantCommands, CloudTlsArg,
    CloudTokenCommands,
};
pub use index::IndexCommands;
pub use mcp::McpCommands;
pub use node::{NodeCommands, WebDevicesCommands};
pub use peer::PeerCommands;
pub use team::TeamCommands;
pub use tool::ToolCommands;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use std::path::PathBuf;
use sven_config::AgentMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormatArg {
    /// Full conversation format (## User / ## Sven / ## Tool / ## Tool Result).
    /// Output is valid sven conversation markdown and fully pipeable.
    #[default]
    Conversation,
    /// Structured JSON: the run's full ATIF trajectory document (schema
    /// version, agent profile, and every step), pretty-printed to stdout.
    /// Not designed for piping between sven instances; use --output-format
    /// jsonl for that, or --output-trace/--trace to write the same
    /// document to a file.
    Json,
    /// Compact plain text: only the final agent response for each step.
    /// Matches the legacy pre-enhancement behaviour.
    Compact,
    /// Full-fidelity JSONL: one ATIF trajectory step (TraceStep) JSON object
    /// per line, streamed as each step is known complete (messages,
    /// thinking, tool calls all folded into their turn-shaped step).
    /// Designed for piping between sven instances:
    ///   sven 'task 1' --output-format jsonl | sven 'task 2'
    /// The receiving sven instance automatically detects and loads the
    /// history. See --output-trace/--trace to persist the equivalent full
    /// trajectory document to a file instead of streaming step-by-step.
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

    /// Optional initial prompt or task description.
    /// When stdin is piped and a prompt is given, stdin is appended to the prompt
    /// with a blank line and sent as one user message (e.g. `cmd | sven "fix these errors"`).
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

    /// Attach an image or audio file to the first user turn.
    ///
    /// Repeatable: `--attach scene.png --attach instruction.wav`.
    /// Images are sent as image content; audio is sent as audio when the model
    /// accepts it and transcribed to text otherwise.  This needs no tool call,
    /// so it also works with models that cannot call tools.
    #[arg(long = "attach", value_name = "PATH")]
    pub attach: Vec<std::path::PathBuf>,

    /// Path to a markdown workflow file (CI mode).
    /// Workflow structure (H1, preamble, `##` steps) is only applied when using
    /// this flag; stdin is never parsed as a workflow.
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

    /// Suppress Sven's built-in system prompt (identity, guidelines, project/
    /// git/CI context, skills, agents, knowledge). With no other prompt flag,
    /// the session starts with zero system tokens before the first message.
    /// Composes with --system-prompt-file / --append-system-prompt: the
    /// supplied text is still sent verbatim, just without Sven's own prompt
    /// wrapped around it.
    #[arg(long)]
    pub no_system: bool,

    /// Disable all tools for this session. No tool schemas are sent to the
    /// model (it has no way to know any tool exists) and any tool call it
    /// attempts anyway is refused. Use this to fit a small-context model
    /// that can't afford the tool schemas' token cost.
    #[arg(long)]
    pub no_tools: bool,

    /// Shorthand for --no-system --no-tools: the absolute minimal request,
    /// just the conversation messages and nothing else.
    #[arg(long)]
    pub bare: bool,

    /// Write the final agent response to a file after the run completes.
    /// The file is created (and intermediate directories) if needed.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output_last_message: Option<PathBuf>,

    /// Load conversation history from a saved ATIF trajectory file before running.
    /// The file is parsed as a full-fidelity ATIF trajectory document; the history
    /// seeds the agent and any workflow steps run on top of it.
    /// Cannot be combined with --trace.
    #[arg(long, value_name = "PATH", conflicts_with = "trace")]
    pub load_trace: Option<PathBuf>,

    /// Write the ATIF trajectory to this path after the run.
    /// If omitted, output goes to the auto-log path (.sven/logs/<timestamp>.atif.json).
    /// Cannot be combined with --trace.
    #[arg(long, value_name = "PATH", conflicts_with = "trace")]
    pub output_trace: Option<PathBuf>,

    /// Combined load + output trace: equivalent to --load-trace PATH --output-trace PATH.
    /// Loads an existing ATIF trajectory from PATH, runs, and writes back to the same file.
    /// This is the ONE session-persistence flag for both headless runs and
    /// interactive TUI/GUI launches - in TUI/GUI mode the file is kept in
    /// sync after every turn. If the file does not exist it is created
    /// automatically with a fresh session ID.
    #[arg(long, value_name = "PATH")]
    pub trace: Option<PathBuf>,

    /// Replay all tool calls recorded in the loaded trajectory with fresh
    /// results before submitting to the model.  Requires --load-trace or --trace.
    #[arg(long)]
    pub rerun_toolcalls: bool,

    /// When loading a conversation with --load-trace or --trace, regenerate
    /// the system prompt from the current skills and config instead of
    /// reusing a stored one.
    ///
    /// Note: ATIF trajectories never persist the system prompt as a step -
    /// the agent always regenerates it fresh on load, so this flag currently
    /// has no additional effect on trace-loaded runs. It is kept for CLI
    /// compatibility and in case a future convention restores stored-system-
    /// prompt reuse.
    #[arg(long)]
    pub regen_system_prompt: bool,

    /// Maximum total tokens (input + output) for the entire run.
    /// When this budget is reached the runner exits with code 4.
    /// 0 or omitted means unlimited.
    ///
    /// Useful in CI pipelines where token spend must be bounded:
    ///   sven --max-tokens 50000 'review all changed files'
    #[arg(long, value_name = "TOKENS")]
    pub max_tokens: Option<u64>,

    /// Increase verbosity (-v = debug, -vv = trace)
    #[arg(long, short = 'v', action = clap::ArgAction::Count)]
    pub verbose: u8,

    // ── Teammate mode (injected by spawn_teammate, not for direct user use) ──
    /// Join a team as a teammate and execute tasks from the shared task list.
    ///
    /// When set, the process enters a polling loop: it claims pending tasks
    /// assigned to `--agent-name`, executes each as a headless agent run, and
    /// marks them complete.  The loop exits when the team config marks this
    /// agent as "closed" (via shutdown_teammate) or when the team directory
    /// disappears.
    ///
    /// This flag is set automatically by `spawn_teammate`; users should not
    /// invoke it directly.
    #[arg(long, hide = true)]
    pub team_name: Option<String>,

    /// Role hint for team membership (teammate, implementer, reviewer, etc.).
    /// Stored in the team config roster for the lead's reference.
    #[arg(long, hide = true)]
    pub team_role: Option<String>,

    /// Peer ID of the team lead.  Informational - used to find the team config.
    #[arg(long, hide = true)]
    pub team_lead_peer: Option<String>,

    /// Name for this agent within the team.  Used for task assignment matching.
    #[arg(long, hide = true, alias = "agent-name")]
    pub teammate_name: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Inspect and call built-in tools directly.
    ///
    /// Useful for scripting, debugging tool behaviour, or quick one-off
    /// operations without starting an agent session.
    ///
    ///   sven tool list                     - list all tools
    ///   sven tool call grep --help         - show grep's parameter schema
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

    /// Expose sven as an ACP agent for JetBrains, Zed, VS Code, and other
    /// ACP-compatible IDEs.
    ///
    /// Run `sven acp serve` to start the agent server.  The process blocks on
    /// stdin/stdout until the IDE disconnects.
    Acp {
        #[command(subcommand)]
        command: AcpCommands,
    },

    /// Node: start the agent, pair devices, manage tokens.
    ///
    /// Run `sven node start` to expose this agent to mobile apps, Slack,
    /// and other clients. The node prints a pairing QR / `sven://` URI at
    /// startup; open it with `sven connect <uri>` (or the mobile app) to
    /// pair, or authorize a device's own URI with `sven node authorize`.
    Node {
        #[command(subcommand)]
        command: NodeCommands,
    },

    /// Connect to a paired node over P2P and steer its session.
    ///
    /// Scan the QR (or copy the `sven://…` URI) a node prints via
    /// `sven node pair`, then:
    ///
    ///   sven connect "sven://<node-id>/<addr>?t=<token>"
    ///
    /// The first connection redeems the one-time token to pair this device
    /// (its key is added to the node's allowlist); afterwards reconnect with the
    /// same `--identity` and no token. You see the conversation on connect and
    /// can send messages; `--message` sends one turn and exits.
    Connect {
        /// The `sven://…` pairing URI from the node's QR code.
        uri: String,
        /// Persist this client's identity keypair here so reconnects
        /// authenticate as the same paired device.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Send a single message and exit (non-interactive; for demos/scripts).
        #[arg(long)]
        message: Option<String>,
    },

    /// Peer: list agents, chat, and search conversation history.
    ///
    /// Starts an ephemeral P2P connection - no running node required.
    ///
    ///   sven peer list                              - discover connected peers
    ///   sven peer chat backend-agent                - interactive chat session
    ///   sven peer search backend-agent "auth"       - grep conversation history
    ///   sven peer search --all "(?i)out.of.memory"  - search across all peers
    Peer {
        #[command(subcommand)]
        command: PeerCommands,
    },

    /// Manage agent teams.
    ///
    ///   sven team list                     - list all teams
    ///   sven team status <NAME>            - detailed team status
    ///   sven team create --name <N>        - create a new team
    ///   sven team start --file team.yaml   - spawn agents from definition
    ///   sven team cleanup <NAME> --force   - remove team data
    ///   sven team definitions              - list project team YAML files
    ///   sven team init --name <N>          - generate a starter definition
    Team {
        #[command(subcommand)]
        command: TeamCommands,
    },

    /// Operate the managed-agents cloud control plane.
    ///
    ///   sven cloud tenant create Acme               - create a tenant
    ///   sven cloud token mint --tenant acme --role companion
    ///   sven cloud serve                            - start the tether endpoint
    ///   sven cloud session start --tenant acme --prompt "..."
    Cloud {
        #[command(subcommand)]
        command: CloudCommands,
    },

    /// Share this workspace's local session so a remote consultant can steer it.
    ///
    /// "Local brain, remote steer": builds a persistent kernel here (like
    /// `sven node start`) and bridges it onto a control plane's `/share`
    /// endpoint. A consultant attaches with
    /// `sven cloud session attach --share-id <id>` and drives the session
    /// against YOUR machine — tools run locally, credentials never leave.
    ///
    ///   sven -c sven.yaml share --url https://cloud.example.com \
    ///     --token "$(cat tenant.token)" --tenant-id acme --share-id debug-1
    Share {
        /// Control-plane base URL, e.g. `https://cloud.example.com`.
        #[arg(long, env = "SVEN_CLOUD_URL", default_value = "https://localhost:8443")]
        url: String,
        /// Tenant bearer token (any valid token of the tenant).
        #[arg(long, env = "SVEN_CLOUD_TOKEN")]
        token: String,
        /// Tenant that owns this share (must match the token's tenant).
        #[arg(long, env = "SVEN_CLOUD_TENANT")]
        tenant_id: String,
        /// Stable id a consultant attaches by. Defaults to a random id (printed
        /// on startup).
        #[arg(long)]
        share_id: Option<String>,
        /// Human-readable label shown to the consultant.
        #[arg(long, default_value = "shared sven session")]
        title: String,
        /// Agent mode for the shared kernel.
        #[arg(long, default_value = "agent")]
        mode: String,
        /// Extra CA certificate PEM to trust (for a self-signed control plane).
        #[arg(long, value_name = "PEM")]
        ca_cert: Option<PathBuf>,
        /// Disable TLS verification — local testing only.
        #[arg(long)]
        insecure: bool,
    },

    /// Generate shell completion script
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Print the effective configuration and exit
    ShowConfig,
    /// Handle OAuth callback from sven:// protocol (used by OS protocol handler).
    ///
    /// When the OAuth server redirects to sven://sven.mcp/callback?code=...&state=...,
    /// the OS invokes this command. It forwards the callback to the local server
    /// listening on port 5598 (or SVEN_OAUTH_CALLBACK_PORT).
    ///
    /// This is normally invoked automatically by the sven:// protocol handler
    /// installed by the Debian package. You only need to call it manually when
    /// testing or when using a custom protocol setup.
    OauthCallback {
        /// The sven:// URL from the OAuth redirect (e.g. sven://sven.mcp/callback?code=X&state=Y).
        #[arg(value_name = "URL")]
        url: String,
    },
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
    /// Build and query a repository context index.
    ///
    /// The index captures the file tree, public API symbols, and import graph.
    /// It is stored in `.sven/index/index.json` inside the repository root.
    ///
    ///   sven index build          - build or rebuild the index
    ///   sven index query "auth"   - find symbols related to auth
    ///   sven index stats          - show index statistics
    Index {
        #[command(subcommand)]
        command: IndexCommands,
    },

    /// Map: run one sven agent per stdin line in parallel.
    ///
    /// Each non-empty line from stdin is substituted for `{}` in the template
    /// and passed to a fresh sven agent.  Agents run in parallel (bounded by
    /// `--concurrency`).  Results are written to stdout in input order,
    /// separated by `---`.
    ///
    /// Examples:
    ///
    ///   git diff --name-only HEAD~1 | sven map 'review {} for bugs'
    ///   cat files.txt | sven map --concurrency 8 'summarise {}'
    ///   ls src/*.rs | sven map --model anthropic/claude-haiku-4-5 'count todos in {}'
    Map {
        /// Template string. `{}` is replaced with each stdin line.
        #[arg(value_name = "TEMPLATE")]
        template: String,
        /// Maximum simultaneous agent instances (default: 4).
        #[arg(long, default_value = "4")]
        concurrency: usize,
        /// Model override forwarded to each child agent.
        #[arg(long, short = 'M', env = "SVEN_MODEL")]
        model: Option<String>,
        /// Output format for each child agent (default: compact).
        #[arg(long, default_value = "compact")]
        output_format: String,
        /// Separator written between sections in the combined output.
        #[arg(long)]
        separator: Option<String>,
    },

    /// Tee: broadcast stdin to N parallel shell commands and merge results.
    ///
    /// Each argument is a shell command that receives an identical copy of stdin.
    /// Outputs are collected in order and written to stdout separated by `---`.
    ///
    /// Examples:
    ///
    ///   sven 'analyze auth module' --output-format compact \
    ///     | sven tee \
    ///         "sven 'find security issues'" \
    ///         "sven 'find performance issues'"
    ///
    ///   cat spec.md | sven tee \
    ///       "sven --mode plan 'make a plan'" \
    ///       "sven --mode research 'research patterns'"
    Tee {
        /// Shell commands to execute in parallel.  Each receives the same stdin.
        #[arg(value_name = "COMMAND", required = true)]
        commands: Vec<String>,
        /// Shell to use for executing commands (default: sh).
        #[arg(long, default_value = "sh")]
        shell: String,
        /// Separator written between sections in the combined output.
        #[arg(long)]
        separator: Option<String>,
    },

    /// Reduce: aggregate stdin sections into one synthesis agent.
    ///
    /// Reads all of stdin and passes it as context to a sven agent together
    /// with the synthesis prompt.  The agent's response is the final output.
    ///
    /// Typically used at the end of a `map` or `tee` pipeline:
    ///
    ///   git diff --name-only HEAD~1 \
    ///     | sven map 'review {} for security issues' \
    ///     | sven reduce 'prioritise these findings and write a report'
    Reduce {
        /// Synthesis prompt sent to the aggregation agent.
        #[arg(value_name = "PROMPT")]
        prompt: String,
        /// Model override for the synthesis agent.
        #[arg(long, short = 'M', env = "SVEN_MODEL")]
        model: Option<String>,
        /// Output format for the synthesis agent (default: compact).
        #[arg(long, default_value = "compact")]
        output_format: String,
        /// Optional preamble prepended before the collected sections.
        #[arg(long)]
        preamble: Option<String>,
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
    /// - positional prompt (e.g. `sven "something"` - one-shot prompt implies headless)
    /// - stdin is not a terminal (piped input, e.g. `echo "task" | sven`)
    /// - stdout is not a terminal (piped output, e.g. `sven 'hi' | sven 'follow up'`)
    ///
    /// Checking stdout matters for the pipe case: the left side of a pipe has
    /// a TTY stdin but a piped stdout.  Without this check it would try to start
    /// the full TUI and write escape codes into the pipe, causing it to hang.
    pub fn is_headless(&self) -> bool {
        self.headless
            || self.prompt.is_some()
            || !std::io::stdin().is_terminal()
            || !std::io::stdout().is_terminal()
    }

    /// Resolve the effective trace input path: --load-trace takes priority, then --trace.
    pub fn effective_load_trace(&self) -> Option<&PathBuf> {
        self.load_trace.as_ref().or(self.trace.as_ref())
    }

    /// Resolve the effective trace output path: --output-trace takes priority, then --trace.
    pub fn effective_output_trace(&self) -> Option<&PathBuf> {
        self.output_trace.as_ref().or(self.trace.as_ref())
    }
}

pub fn print_completions(shell: Shell) {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "sven", &mut std::io::stdout());
}

// TTY detection re-uses the stdlib IsTerminal trait (stable since Rust 1.70).
// Import it into scope so callers can call .is_terminal() on Stdin/Stdout.
use std::io::IsTerminal as _;
