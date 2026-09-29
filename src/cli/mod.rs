// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven` CLI surface: top-level [`Cli`] args, the [`Commands`] dispatch
//! enum, and one module per subcommand group's own nested `*Commands` enum.
//!
//! This mirrors `src/run/` (see `src/main.rs`), which holds the *handler*
//! for each of these groups — this module only declares the clap grammar.

mod agent;
mod index;
#[cfg(feature = "memory")]
mod learn;
#[cfg(feature = "memory")]
mod questions;
mod task;
#[cfg(feature = "network")]
mod team;
mod tool;

// `AcpCommands`/`McpCommands` live in `sven-acp`/`sven-mcp` themselves,
// shared verbatim with the standalone `sven-acp`/`sven-mcp` binaries rather
// than duplicated here.
pub use agent::AgentCommands;
pub use index::IndexCommands;
#[cfg(feature = "memory")]
pub use learn::LearnCommands;
#[cfg(feature = "memory")]
pub use questions::QuestionsCommands;
#[cfg(feature = "network")]
pub use sven_acp::cli::AcpCommands;
#[cfg(feature = "network")]
pub use sven_mcp::cli::McpCommands;
pub use task::TaskCommands;
#[cfg(feature = "network")]
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
    ///   sven 'task 1' --output-format jsonl | sven --stdin 'task 2'
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
    /// A piped stdin is read only when there is no PROMPT (stdin is then the
    /// task) or when --stdin asks for it as additional context.
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Read stdin to end and append it to PROMPT with a blank line, as one
    /// user message (`cmd | sven --stdin "fix these errors"`). Without it, a
    /// run with a PROMPT never reads or waits on stdin.
    #[arg(long)]
    pub stdin: bool,

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
    //
    // No `no-nvim` alias: clap maps an alias onto the *same* bool, so
    // `--no-nvim` switched Neovim on. It appeared in `sven completions`, so
    // users found and used it.
    #[arg(long)]
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

    /// Run an agent one step at a time, keeping its state in a file.
    ///
    /// The shell-level form of the sven SDK: each invocation loads the agent,
    /// advances it by exactly one step, persists, and exits - the same shape a
    /// service uses per request.
    ///
    ///   sven agent step "what does this repo do?"
    ///   sven agent step --state ./s.json "read src/lib.rs and summarise it"
    ///   sven agent step --state ./s.json "now list its public types"
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },

    /// Expose sven as an MCP server for use with Cursor, Claude Desktop, and
    /// other MCP-compatible hosts.
    ///
    /// Run `sven mcp serve` to start the server.  The process blocks on
    /// stdin/stdout until the host disconnects.
    #[cfg(feature = "network")]
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },

    /// Expose sven as an ACP agent for JetBrains, Zed, VS Code, and other
    /// ACP-compatible IDEs.
    ///
    /// Run `sven acp serve` to start the agent server.  The process blocks on
    /// stdin/stdout until the IDE disconnects.
    #[cfg(feature = "network")]
    Acp {
        #[command(subcommand)]
        command: AcpCommands,
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
    #[cfg(feature = "network")]
    Team {
        #[command(subcommand)]
        command: TeamCommands,
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
    /// One-shot bulk migration of legacy `.yaml` chat files to the ATIF
    /// `.json` trajectory format.
    ///
    /// Sven already migrates a legacy chat automatically the first time it's
    /// opened (`--resume`, or picked in the TUI) - this command just runs
    /// that same conversion for every legacy chat at once instead of one at
    /// a time. Idempotent: never overwrites a session that already has a
    /// `.json` file, and never touches the original `.yaml` files.
    MigrateSessions {
        /// Report what would be migrated without writing anything.
        #[arg(long)]
        dry_run: bool,
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

    /// Drive the continuous-learning drain by hand.
    ///
    /// Facts sven recorded as durable knowledge normally reach training on a
    /// timer, in the background of an open session. `sven learn flush` is the
    /// version for a script that has no next tick: it submits everything
    /// pending and blocks until every fact has a real outcome.
    ///
    ///   sven learn flush          - drain now, block, report per fact
    #[cfg(feature = "memory")]
    Learn {
        #[command(subcommand)]
        command: LearnCommands,
    },

    /// See and answer questions a headless run parked instead of guessing.
    ///
    /// A run that cannot answer a question itself exits with code 5 rather
    /// than block or fabricate an answer. `sven questions list` shows what is
    /// waiting; `sven questions answer` durably records a human's reply.
    ///
    ///   sven questions list
    ///   sven questions answer <id> "Axum"
    #[cfg(feature = "memory")]
    Questions {
        #[command(subcommand)]
        command: QuestionsCommands,
    },

    /// Attempt a task whose completion a declarative verifier checks, not
    /// the model's own claim of being done.
    ///
    ///   sven task run my-task.task.toml
    Task {
        #[command(subcommand)]
        command: TaskCommands,
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

    /// Handle one agent-dispatch stdio request.
    ///
    /// Reads exactly one JSON request object from stdin
    /// (`{"mode", "device", "params"}`, the agent-dispatch stdio contract;
    /// see `.agents/roadmap/android-ui-test.md`), then closes stdin. Runs the matching sven machine to completion through
    /// the same `RuntimeBuilder`/mode-registry path every other sven
    /// machine uses, and writes exactly one JSON reply as the LAST line of
    /// stdout: `{"ok": true, "output": <any JSON>}` on success or
    /// `{"ok": false, "error": "<message>"}` on failure.
    ///
    /// Exit code 0 covers BOTH outcomes above - a failed UI-test step is
    /// still a well-formed reply, not a process fault. A non-zero exit is
    /// reserved for a genuine subcommand-level fault: malformed stdin, or an
    /// internal error building/joining the kernel session.
    ///
    ///   echo '{"mode": "ui-test", "params": {...}}' | sven agent-dispatch
    AgentDispatch,

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
    /// - stdout is not a terminal (piped output, e.g. `sven 'hi' | sven --stdin 'follow up'`)
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

    /// Whether a headless run reads stdin: when --stdin asks for it, or when
    /// there is no PROMPT and stdin is not a terminal, so stdin is the task.
    /// Deciding from the arguments alone means an inherited pipe that nobody
    /// writes to or closes can never stall a run that already has its task.
    pub fn reads_stdin(&self) -> bool {
        self.stdin || (self.prompt.is_none() && !std::io::stdin().is_terminal())
    }

    /// Resolve the effective trace input path: --load-trace takes priority, then --trace.
    ///
    /// `--trace` on a missing file starts a fresh session there, but
    /// `--load-trace` only reads: a missing file is an error rather than a
    /// silent fresh start, which would run the task without its history.
    pub fn effective_load_trace(&self) -> anyhow::Result<Option<&PathBuf>> {
        if let Some(path) = &self.load_trace {
            anyhow::ensure!(
                path.exists(),
                "--load-trace {}: no such file",
                path.display()
            );
        }
        Ok(self.load_trace.as_ref().or(self.trace.as_ref()))
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
