// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven harness` grammar: validate, inspect, compile, explain, run,
//! resume and replay programmable harnesses.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

/// How `inspect --graph` draws a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GraphArg {
    /// Graphviz DOT.
    Dot,
    /// A Mermaid flowchart.
    Mermaid,
}

/// How `compile` writes the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PlanFormatArg {
    /// Canonical JSON: sorted keys, the form the digest is taken over.
    Json,
    /// The same plan as YAML.
    Yaml,
}

/// Whether the calls of a production run wait for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApprovalsArg {
    /// Every call the harness and the host ceiling allow runs unasked.
    Auto,
    /// Every call that is not read-only is refused; nobody is asked.
    Deny,
    /// Every call that is not read-only is put to you on the terminal
    /// (stderr, answered on stdin); no answer is a refusal.
    Ask,
}

/// `sven harness` subcommands; the exit codes are on `sven harness --help`.
#[derive(Subcommand, Debug)]
pub enum HarnessCommands {
    /// Compile harnesses and the workflows they import without running them.
    ///
    /// PATH is a harness file, or a directory holding `harness.yaml`.
    Validate {
        /// Harness files or directories.
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Print the reports as one JSON array on stdout.
        #[arg(long)]
        json: bool,
    },

    /// Show what a harness is: steps, capabilities, limits, digests and
    /// imports; or with `--graph` draw its plan.
    ///
    /// A workflow's graph groups the generated steps by authored
    /// task and captions them by role, check and attempt.
    Inspect {
        /// A harness file or directory.
        path: PathBuf,
        /// Print the report as JSON on stdout.
        #[arg(long)]
        json: bool,
        /// Print the plan as a graph instead of the report.
        #[arg(long, value_enum, value_name = "FORMAT", conflicts_with = "json")]
        graph: Option<GraphArg>,
    },

    /// Print the compiled plan: canonical JSON (the form the digest covers)
    /// or YAML, with the source map and profile. Exit 1 when the document
    /// does not compile.
    Compile {
        /// A harness file or directory.
        path: PathBuf,
        /// The output format.
        #[arg(long, value_enum, value_name = "FORMAT", default_value = "json")]
        format: PlanFormatArg,
        /// Write the plan here instead of stdout.
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Explain a harness in the author's terms: a workflow task by task,
    /// with its checks, repairs and success condition. With `--step ID`,
    /// explain one generated step: its origin, where it leads and from where
    /// it is reached.
    Explain {
        /// A harness file or directory.
        path: PathBuf,
        /// A step id of the compiled plan (see `inspect`).
        #[arg(long, value_name = "ID")]
        step: Option<String>,
        /// Inputs as a JSON object: also print each prompt, system message
        /// and gate command as a run with these inputs would send it
        /// (values are data; holes a run fills later stay visible).
        #[arg(long, value_name = "JSON", conflicts_with = "input_file")]
        input: Option<String>,
        /// The inputs from a JSON file.
        #[arg(long, value_name = "FILE")]
        input_file: Option<PathBuf>,
        /// Cut each rendered text to this many characters.
        #[arg(long, value_name = "N", default_value_t = 400)]
        elide: usize,
    },

    /// Say where a suspended run rests: its step (and task, check and
    /// attempt), counters, completed steps, loop progress, and whether the
    /// document still matches the checkpoint. Exit 1 when `resume` would
    /// refuse the checkpoint, 2 when the file is not a checkpoint.
    Status {
        /// A harness file or directory.
        path: PathBuf,
        /// The checkpoint written by `run --checkpoint-out`.
        #[arg(long, value_name = "FILE")]
        checkpoint: PathBuf,
        /// Print the report as JSON on stdout.
        #[arg(long)]
        json: bool,
    },

    /// Print a JSON Schema: the harness document schema, or with PATH that
    /// harness's input/output contract.
    Schema {
        /// A harness file or directory; omit for the document schema.
        path: Option<PathBuf>,
    },

    /// Run a harness once.
    ///
    /// The run uses a real model and sven's real coding tools, and four flags
    /// are required because nothing that widens a run is implied:
    /// `--workspace DIR` (where the file tools are confined and the shell
    /// starts), `--model <provider/name>` and `--model-endpoint <url>` of a
    /// server you already run (sven never starts or pulls a model), and
    /// `--approvals auto|deny|ask`. sven reads no configuration or
    /// environment here.
    ///
    /// Progress lines `[sven:harness] {...}` go to stderr as the run goes,
    /// unless --quiet. --json prints one JSON document on stdout and nothing
    /// else.
    Run {
        /// A harness file or directory.
        path: PathBuf,
        #[command(flatten)]
        run: HarnessRunArgs,
    },

    /// Resume a suspended run from its checkpoint.
    ///
    /// Takes the same flags as `run`; the
    /// checkpoint carries the inputs. A checkpoint taken under another
    /// program is refused (exit 2).
    Resume {
        /// A harness file or directory.
        path: PathBuf,
        /// The checkpoint file written by `run --checkpoint-out`.
        #[arg(long, value_name = "FILE")]
        checkpoint: PathBuf,
        #[command(flatten)]
        run: HarnessRunArgs,
    },

    /// Replay a recording offline: no model, tool or child is started.
    Replay {
        /// A harness file or directory.
        path: PathBuf,
        /// The recording written by `run --record`.
        #[arg(long, value_name = "FILE")]
        recording: PathBuf,
        /// A JSONL trace (`run --trace`) the replay's trace must equal.
        #[arg(long, value_name = "FILE")]
        expect_trace: Option<PathBuf>,
        /// Print the replay as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
}

/// What `run` and `resume` share: where the run's world comes from, its
/// bounds and the files it writes.
#[derive(Args, Debug)]
pub struct HarnessRunArgs {
    /// The run's inputs as a JSON object.
    #[arg(long, value_name = "JSON", conflicts_with = "input_file")]
    pub input: Option<String>,
    /// The run's inputs from a JSON file.
    #[arg(long, value_name = "FILE")]
    pub input_file: Option<PathBuf>,
    /// The directory the agent works in: the project root, where the file
    /// tools are confined and the shell starts. Required.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
    /// The run's private directory, where its audit log and its gates' files
    /// go (created if missing, mode 0700). Default: the harness's
    /// `execution.scratchpad`, else a fresh `sven-harness-<run id>` under the
    /// system temporary directory; on `resume`, the one the checkpoint
    /// recorded. It may not be inside `--workspace` or contain it. Kept after
    /// the run; its path is printed.
    #[arg(long, value_name = "DIR")]
    pub scratchpad: Option<PathBuf>,
    /// The model, `provider/name`. Required, with `--model-endpoint`.
    #[arg(long, value_name = "SPEC")]
    pub model: Option<String>,
    /// Base URL of the model server you already run. Required.
    #[arg(long, value_name = "URL", requires = "model")]
    pub model_endpoint: Option<String>,
    /// The model that answers a role the harness names (`agent.model` or a
    /// task's `model`), `ROLE=provider/name` (repeatable). It is served by
    /// `--model-endpoint` with the key `--api-key-file` holds for its
    /// provider. Every role the workflow names must be mapped, and a mapped
    /// role it does not name is refused as a likely typo.
    #[arg(long = "model-role", value_name = "ROLE=SPEC", requires = "model")]
    pub model_roles: Vec<String>,
    /// Whether calls wait for you. Required: a run never inherits an
    /// approval mode.
    #[arg(long, value_enum, value_name = "MODE")]
    pub approvals: Option<ApprovalsArg>,
    /// A capability the host grants the run (repeatable). A run holds a capability only when its harness
    /// declares it AND the host grants it. Default: ReadFile, WriteFile,
    /// DeleteFile, ExecuteShell, RunVerifier and SpawnChild - never
    /// NetworkAccess, GitOperation or ControlDevice unless named here.
    #[arg(
        long = "allow-capability",
        value_name = "NAME",
        value_parser = clap::builder::PossibleValuesParser::new(sven_harness_spec::CapabilityName::known().to_vec())
    )]
    pub allow_capabilities: Vec<String>,
    /// Seconds to wait for the model server's first response bytes before a
    /// model call fails. Raise it for a server
    /// that queues requests.
    #[arg(long, value_name = "SECS")]
    pub first_byte_timeout: Option<u64>,
    /// A file holding the model server's API key, read once and handed to
    /// the model client only: a JSON object of provider name to key, or the
    /// key itself on one line. Optional (a local server may need none).
    /// Refused when the file is inside `--workspace`, where the agent could
    /// read it. Needs `--model`.
    #[arg(long, value_name = "FILE", requires = "model")]
    pub api_key_file: Option<PathBuf>,
    /// Suspend the run (leaving a checkpoint) after this many seconds.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,
    /// Suspend the run once the model has generated this many tokens.
    #[arg(long, value_name = "N")]
    pub max_output_tokens: Option<u64>,
    /// Write the trace as JSONL (one trace record per line).
    #[arg(long, value_name = "FILE")]
    pub trace: Option<PathBuf>,
    /// Write the recording (for `replay`) as JSON.
    #[arg(long, value_name = "FILE")]
    pub record: Option<PathBuf>,
    /// Write the ATIF trajectory as JSON.
    #[arg(long, value_name = "FILE")]
    pub trajectory: Option<PathBuf>,
    /// Write each conversation of the run (one agent's work: a task, or one
    /// role of one loop item) as its own ATIF trajectory, `NNNN-<name>.json`,
    /// into DIR: tool-call training data, kept only when asked for.
    #[arg(long, value_name = "DIR")]
    pub conversations: Option<PathBuf>,
    /// Write the checkpoint of a suspended run as JSON.
    #[arg(long, value_name = "FILE")]
    pub checkpoint_out: Option<PathBuf>,
    /// Print the result as one JSON document on stdout.
    #[arg(long)]
    pub json: bool,
    /// No `[sven:harness]` progress lines on stderr.
    #[arg(long)]
    pub quiet: bool,
}
