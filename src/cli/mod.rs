// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven` CLI surface: top-level [`Cli`] args, the [`Commands`] dispatch
//! enum, and one module per subcommand group's own nested `*Commands` enum.
//!
//! This mirrors `src/run/` (see `src/main.rs`), which holds the *handler*
//! for each of these groups - this module only declares the clap grammar.
//!
//! Doc comments here are clap's `--help` text, printed verbatim, so their
//! `<NAME>` placeholders are usage syntax, not HTML; rustdoc is told so.
#![allow(rustdoc::invalid_html_tags)]

mod agent;
mod assembly;
mod auth;
mod commands;
mod config;
mod decisions;
mod disclosure;
#[cfg(feature = "document")]
mod document;
mod harness;
mod index;
mod media;
mod output_format;
mod questions;
#[cfg(feature = "network")]
mod rc;
#[cfg(feature = "network")]
pub use rc::RcAction;
mod replay;
mod task;
#[cfg(feature = "network")]
mod team;
mod tool;

pub use assembly::ContextAssemblyArg;
pub use disclosure::DisclosureFlags;

// `AcpCommands`/`McpCommands` live in `sven-acp`/`sven-mcp` themselves,
// shared verbatim with the standalone `sven-acp`/`sven-mcp` binaries rather
// than duplicated here.
pub use agent::AgentCommands;
pub use auth::AuthCommands;
pub use commands::Commands;
pub use config::ConfigCommands;
pub use decisions::{DecisionsCommands, DoctorCommands};
#[cfg(feature = "document")]
pub use document::{ConvertArgs, DocumentCommands};
pub use harness::{ApprovalsArg, GraphArg, HarnessCommands, HarnessRunArgs, PlanFormatArg};
pub use index::IndexCommands;
pub use media::{GenerateArgs, MediaCommands};
pub use output_format::OutputFormatArg;
pub use questions::QuestionsCommands;
pub use replay::ReplayArgs;
#[cfg(feature = "network")]
pub use sven_acp::cli::AcpCommands;
#[cfg(feature = "network")]
pub use sven_mcp::cli::McpCommands;
pub use task::TaskCommands;
#[cfg(feature = "network")]
pub use team::TeamCommands;
pub use tool::ToolCommands;

use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use std::path::PathBuf;
use sven_vocab::{AgentMode, ApprovalMode};

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

    /// Agent mode: what the agent may do at all (research and plan never
    /// write). Default: `agent.default_mode` of the configuration, else agent.
    #[arg(long, short = 'm', value_enum)]
    pub mode: Option<AgentMode>,

    /// Whether tool calls wait for your approval.
    ///
    /// `auto` (the default) runs every call the mode allows - shell and file
    /// writes included - without asking. `manual` shows you each call that is
    /// not read-only, with the tool and its command, path or arguments, and
    /// runs it only once you approve; a `task` sub-agent's and an SDLC task's
    /// calls come to the same prompt. Manual approval needs the interactive
    /// TUI: sven refuses to start it headless, in a pipe, or as a team member.
    #[arg(long, value_enum, default_value = "auto")]
    pub approval: ApprovalMode,

    #[command(flatten)]
    pub disclosure: DisclosureFlags,

    /// How the context of a request is assembled from the conversation,
    /// overriding `context.assembly.mode` of the configuration file for this
    /// process.
    ///
    /// `off` sends the conversation as it is. `demote` sends the tool results of
    /// older rounds in a smaller form once the conversation outgrows its token
    /// budget; the model can expand a shortened result back to its raw text.
    /// `reconstruct` does that and starts a resumed session from a rebuilt
    /// history instead of the whole transcript.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        overrides_with = "context_assembly"
    )]
    pub context_assembly: Option<ContextAssemblyArg>,

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
    /// interactive TUI launches - in TUI mode the file is kept in
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

    /// Print one `[sven:ledger]` line to stderr per model request: a JSON
    /// object attributing the request's input tokens to their sources
    /// (system prompt, each tool schema, user and assistant turns, tool
    /// calls and results, media) and reconciling the estimate with the
    /// tokens the provider reported. The ledger is also saved in the
    /// trajectory; `sven ledger FILE` summarizes it.
    #[arg(long)]
    pub token_ledger: bool,

    /// Run the whole headless prompt as a standing goal with this many turns
    /// (1 to 500), taken verbatim as the objective: quotes, line breaks and
    /// spacing are kept, unlike a `/goal` line, which is read word by word.
    /// The run takes turns and exits as `/goal` does in a headless run (0
    /// completed, 4 out of turns, 5 blocked, 6 paused). A prompt that is
    /// itself a `/goal` line is refused. See docs/guides/goals.md.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=500))]
    pub goal_turns: Option<u32>,

    /// The command that decides whether the `--goal-turns` goal is done: run
    /// in the project after each turn, it passes on exit status 0 (within 600
    /// seconds). Given as one command line, quoted as `/goal --check` takes
    /// it. Needs `--goal-turns`.
    #[arg(long, value_name = "COMMAND", requires = "goal_turns")]
    pub goal_check: Option<String>,

    /// Append every model request this process sends, exactly as sent, and
    /// what came back to FILE (JSONL). `sven replay FILE` sends one again.
    #[arg(long, value_name = "FILE")]
    pub record_requests: Option<PathBuf>,

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

    /// Refuses `--approval manual` anywhere but the interactive TUI: nobody
    /// could answer the prompts, and approving on their behalf is exactly
    /// what manual approval rules out. Checked before anything runs.
    pub fn check_approval(&self) -> anyhow::Result<()> {
        if self.approval != ApprovalMode::Manual {
            return Ok(());
        }
        let not_interactive = if self.command.is_some() {
            Some("a subcommand runs no interactive session")
        } else if self.team_name.is_some() {
            Some("a team member runs unattended")
        } else if self.is_headless() {
            Some("this run is headless (--headless, a PROMPT, or stdin/stdout not a terminal)")
        } else {
            None
        };
        match not_interactive {
            Some(why) => anyhow::bail!(
                "--approval manual needs the interactive TUI on a terminal, and {why}; \
                 run `sven` in a terminal, or drop --approval manual"
            ),
            None => Ok(()),
        }
    }

    /// The goal terms `--goal-turns` and `--goal-check` give a headless run,
    /// `None` without `--goal-turns`.
    ///
    /// # Errors
    /// When the run is not a headless prompt run, or the check holds no
    /// command: refused before anything runs.
    pub fn goal_terms(&self) -> Result<Option<sven_ci::GoalTerms>, String> {
        let Some(max_turns) = self.goal_turns else {
            return Ok(None);
        };
        if self.command.is_some() || self.team_name.is_some() || !self.is_headless() {
            return Err("--goal-turns sets the goal of a headless run: \
                 give a PROMPT or pipe one in, with --headless"
                .to_string());
        }
        let check = self
            .goal_check
            .as_deref()
            .map(sven_frontend::goal::check_from_line)
            .transpose()
            .map_err(|why| format!("--goal-check: {why}"))?;
        Ok(Some(sven_ci::GoalTerms { max_turns, check }))
    }

    /// Whether a headless run reads stdin: when --stdin asks for it, or when
    /// there is no PROMPT and stdin is not a terminal, so stdin is the task.
    /// Deciding from the arguments alone means an inherited pipe that nobody
    /// writes to or closes can never stall a run that already has its task.
    pub fn reads_stdin(&self) -> bool {
        self.stdin || (self.prompt.is_none() && !std::io::stdin().is_terminal())
    }

    /// The mode of this run: `--mode` when given, else the configured
    /// `agent.default_mode`.
    pub fn agent_mode(&self, configured_default: AgentMode) -> AgentMode {
        self.mode.unwrap_or(configured_default)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("sven").chain(args.iter().copied()))
            .expect("the arguments parse")
    }

    /// `sven auth login` signs in to the hosted sven service unless told
    /// another server.
    #[cfg(feature = "network")]
    #[test]
    fn auth_login_defaults_to_the_hosted_service() {
        for (args, expected) in [
            (&["auth", "login"][..], "https://sven.swedishembedded.com"),
            (
                &["auth", "login", "--server", "localhost:3000"],
                "localhost:3000",
            ),
        ] {
            let Some(Commands::Auth {
                command: AuthCommands::Login { server, .. },
            }) = cli(args).command
            else {
                panic!("{args:?} is not `auth login`");
            };
            assert_eq!(server, expected);
        }
    }

    #[cfg(feature = "network")]
    #[test]
    fn auth_login_no_browser_is_opt_in() {
        for (args, expected) in [
            (&["auth", "login"][..], false),
            (&["auth", "login", "--no-browser"], true),
        ] {
            let Some(Commands::Auth {
                command: AuthCommands::Login { no_browser, .. },
            }) = cli(args).command
            else {
                panic!("{args:?} is not `auth login`");
            };
            assert_eq!(no_browser, expected);
        }
    }

    /// A start-credential host is an ordinary `sven rc`, never a hosted one.
    #[cfg(feature = "network")]
    #[test]
    fn rc_login_file_conflicts_with_hosted() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("sven").chain(args.iter().copied()))
        };
        assert!(parse(&["rc", "--login-file", "/run/login.yaml"]).is_ok());
        for hosted in [
            &["--hosted", "--sandbox", "/srv/x"][..],
            &["--hosted", "--container"],
        ] {
            let mut args = vec!["rc", "--login-file", "/run/login.yaml"];
            args.extend_from_slice(hosted);
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    /// An explicit `--mode` wins; without one the configured default applies.
    #[test]
    fn mode_falls_back_to_the_configured_default() {
        assert_eq!(
            cli(&["fix it"]).agent_mode(AgentMode::Plan),
            AgentMode::Plan
        );
        assert_eq!(
            cli(&["--mode", "research", "fix it"]).agent_mode(AgentMode::Plan),
            AgentMode::Research
        );
        assert_eq!(
            cli(&["-m", "agent", "fix it"]).agent_mode(AgentMode::Plan),
            AgentMode::Agent
        );
    }

    /// Manual approval with nobody at a terminal to answer is refused before
    /// anything runs, never left to hang at the first prompt.
    #[test]
    fn manual_approval_is_refused_where_nobody_can_answer() {
        for args in [
            &["--approval", "manual", "--headless", "fix it"][..],
            &["--approval", "manual", "fix it"],
            &[
                "--approval",
                "manual",
                "--team-name",
                "t",
                "--agent-name",
                "a",
            ],
            &["--approval", "manual", "chats"],
        ] {
            let err = cli(args).check_approval().expect_err("refused");
            assert!(
                err.to_string()
                    .contains("--approval manual needs the interactive TUI"),
                "{args:?}: {err}"
            );
        }
    }

    #[test]
    fn goal_flags_give_a_headless_run_its_goal_terms() {
        assert!(cli(&["fix it"]).goal_terms().unwrap().is_none());
        let terms = cli(&["--goal-turns", "9", "--goal-check", "make test", "fix it"])
            .goal_terms()
            .unwrap()
            .expect("terms");
        assert_eq!(terms.max_turns, 9);
        assert_eq!(terms.check.unwrap().argv, ["make", "test"]);
        // Refused before anything runs: out of range, a check without turns,
        // a blank check, a run that is not a headless prompt run.
        for args in [
            &["--goal-turns", "0", "fix it"][..],
            &["--goal-turns", "501", "fix it"],
            &["--goal-turns", "x", "fix it"],
            &["--goal-check", "make test", "fix it"],
        ] {
            assert!(
                Cli::try_parse_from(std::iter::once("sven").chain(args.iter().copied())).is_err(),
                "{args:?}"
            );
        }
        assert!(cli(&["--goal-turns", "3", "--goal-check", " ", "fix it"])
            .goal_terms()
            .is_err());
        assert!(cli(&["--goal-turns", "3", "chats"]).goal_terms().is_err());
    }

    #[test]
    fn auto_approval_runs_anywhere_and_is_the_default() {
        assert_eq!(cli(&["fix it"]).approval, ApprovalMode::Auto);
        for args in [
            &["--headless", "fix it"][..],
            &["--approval", "auto", "chats"],
        ] {
            assert!(cli(args).check_approval().is_ok(), "{args:?}");
        }
    }
}
