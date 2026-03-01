// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use std::path::PathBuf;
use sven_config::AgentMode;

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
    /// and printed once. Mobile clients pair via `sven node pair`.
    Start {
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Authorize a device to control this node via P2P.
    ///
    /// The device displays a `sven-pair://` URI (or QR code). Paste it here.
    /// The peer's PeerId and short fingerprint are shown for visual confirmation.
    Pair {
        /// The `sven-pair://` URI displayed by the device.
        uri: String,
        /// Human-readable label for this device (e.g. "my-phone").
        #[arg(long, short = 'l')]
        label: Option<String>,
        /// Path to the node config file.
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },

    /// Revoke a previously authorized peer.
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

    /// List all authorized operator peers.
    ///
    /// Shows the peers in `authorized_peers.yaml` — the devices authorized
    /// to control this node via P2P.  Use `sven node pair` to add
    /// devices and `sven node revoke` to remove them.
    ListPeers {
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
    /// environment variable or the --token flag.
    ///
    /// Example:
    ///   export SVEN_NODE_TOKEN=<token shown at first startup>
    ///   sven node exec "delegate a task to say hi to agent local"
    Exec {
        /// The task to send to the agent.
        task: String,
        /// Bearer token (or set SVEN_NODE_TOKEN).
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
    /// Node: start the agent, pair devices, manage tokens.
    ///
    /// Run `sven node start` to expose this agent to mobile apps, Slack,
    /// and other clients. Run `sven node pair <uri>` to authorize a device.
    Node {
        #[command(subcommand)]
        command: NodeCommands,
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
