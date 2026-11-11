// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The [`Commands`] dispatch enum: every `sven <subcommand>` and its flags,
//! each group's own nested `*Commands` enum in a module of its own.
//!
//! Doc comments here are clap's `--help` text, printed verbatim, so their
//! `<NAME>` placeholders are usage syntax, not HTML; rustdoc is told so.
#![allow(rustdoc::invalid_html_tags)]

use std::path::PathBuf;

use clap::Subcommand;
use clap_complete::Shell;

#[cfg(feature = "document")]
use super::DocumentCommands;
#[cfg(feature = "network")]
use super::{AcpCommands, McpCommands, TeamCommands};
use super::{
    AgentCommands, AuthCommands, ConfigCommands, DecisionsCommands, DoctorCommands,
    HarnessCommands, IndexCommands, MediaCommands, QuestionsCommands, ReplayArgs, TaskCommands,
    ToolCommands,
};

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

    /// Make video clips with a model; check and join them without decoding.
    Media {
        #[command(subcommand)]
        command: MediaCommands,
    },

    /// Write a starting configuration, `.sven.yaml`, in this directory: an
    /// OpenRouter GLM model and no key (`sven auth set openrouter` stores it).
    /// An existing configuration file here is never overwritten.
    Init,

    /// Read and write one key of a configuration file.
    ///
    ///   sven config get model.name            - the value in force
    ///   sven config set model.temperature 0.3 - write it into ./.sven.yaml
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Store, list and forget the keys of hosted providers.
    ///
    ///   sven auth set openrouter   - store OpenRouter's key (read from stdin)
    ///   sven auth list             - the providers with a stored key
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// Find out why something does not work.
    ///
    ///   sven doctor decisions    - the decision model: readiness, limits, latency
    Doctor {
        #[command(subcommand)]
        command: DoctorCommands,
    },

    /// The decision model that advises the harness.
    ///
    ///   sven decisions models    - the models the provider knows
    Decisions {
        #[command(subcommand)]
        command: DecisionsCommands,
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

    /// Remote control: run sessions here for the server `sven auth login`
    /// signed in to, with this machine's tools, until Ctrl-C.
    ///   sven rc --working-dir ~/src/project
    #[cfg(feature = "network")]
    Rc(super::rc::RcArgs),

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
    /// Send a recorded model request (`--record-requests`) again, to
    /// reproduce what the model did there; no tool runs.
    ///   sven replay run.requests.jsonl --request 14 --repeat 5 --seed 7
    ///   sven replay run.requests.jsonl --list
    ///   sven replay run.requests.jsonl --request 14 --variant terse.yaml --repeat 10 --json
    Replay(ReplayArgs),
    /// Summarize the token ledger of a saved trajectory.
    ///
    /// A run made with `--token-ledger` (or any run, since the ledger is
    /// always saved in the trajectory) records, for every model request, how
    /// many input tokens went to each source. This sums them over the whole
    /// trajectory, subagents included, and prints a table, or with `--json`
    /// one JSON object.
    ///
    ///   sven --token-ledger --output-trace run.json 'fix the bug'
    ///   sven ledger run.json
    Ledger {
        /// The trajectory file (as written by `--output-trace`)
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Print the summary as JSON instead of a table
        #[arg(long)]
        json: bool,
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
    /// Convert PDFs and pictures of pages to Markdown with an OCR model (`convert`, `score`).
    #[cfg(feature = "document")]
    Document {
        #[command(subcommand)]
        command: DocumentCommands,
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

    /// See and answer parked questions.
    ///
    /// A headless run's own questions are answered at once ("no user is
    /// available"). What parks is a question from an embedding host that
    /// chose to park (`EngineBuilder::park_questions`); a verified task whose
    /// verifier needs a person exits with code 5. `sven questions list`
    /// shows what is waiting; `sven questions answer` durably records a
    /// human's reply.
    ///
    ///   sven questions list
    ///   sven questions answer <id> "Axum"
    Questions {
        #[command(subcommand)]
        command: QuestionsCommands,
    },

    /// Validate, inspect, run, resume and replay programmable harnesses.
    ///
    /// The reading commands compile a harness and start nothing. `run` and
    /// `resume` drive a real model on a server you already run, with the
    /// coding tools on a workspace you name and an approval mode you choose;
    /// no sven configuration is read.
    ///
    ///   sven harness validate DIR...
    ///   sven harness run DIR --input '{"k": "v"}' --workspace DIR
    ///                     --model PROVIDER/NAME --model-endpoint URL
    ///                     --approvals auto|deny|ask [--json]
    ///   sven harness resume DIR --checkpoint FILE [the flags of run]
    ///   sven harness replay DIR --recording FILE [--expect-trace FILE]
    ///   sven harness schema (optional output PATH)
    ///   sven harness compile DIR [--format json|yaml] [-o FILE]
    ///   sven harness inspect DIR [--graph dot|mermaid]
    ///   sven harness explain DIR [--step ID]
    ///   sven harness status DIR --checkpoint FILE [--json]
    ///
    /// Exit codes. run, resume: 0 returned, 1 the workflow failed, 2 usage or
    /// invalid harness/input/file, 3 suspended by a bound, 4 cancelled,
    /// 5 infrastructure. validate, inspect, compile: 0 valid, 1 invalid, 2
    /// unreadable path. explain: 0 explained, 2 unknown step or unreadable
    /// path. status: 0 the checkpoint fits the document, 1 resume would refuse
    /// it, 2 not a checkpoint. replay: 0 matched, 1 trace mismatch, 2 refused
    /// recording.
    // Verbatim, so `--help` keeps the examples and exit codes apart.
    #[command(verbatim_doc_comment)]
    Harness {
        #[command(subcommand)]
        command: Box<HarnessCommands>,
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

    /// List the models that can be used now, each with the name `--model`
    /// takes: every local server that answers (brain, ollama, vLLM, LM
    /// Studio), every `providers:` entry and every hosted provider with a
    /// stored key, asked live. A source that does not answer is reported.
    ListModels {
        /// Only this provider: a driver id ("openrouter", "brain") or a
        /// `providers:` entry
        #[arg(long, short = 'p')]
        provider: Option<String>,
        /// Output as JSON instead of a formatted table
        #[arg(long)]
        json: bool,
    },

    /// Handle one agent-dispatch stdio request.
    ///
    /// Reads one JSON request (`{"mode", "device", "params"}`) from stdin,
    /// runs the matching machine to completion, and writes one JSON reply as
    /// the last line of stdout: `{"ok": true, "output": ...}` or
    /// `{"ok": false, "error": "..."}`. Both exit 0; a non-zero exit means
    /// malformed stdin or a session that could not be built. The one mode is
    /// `ui-test` (the `android` feature); any other is answered `ok: false`.
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
