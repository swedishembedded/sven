// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven harness` grammar: validate, inspect, run, resume, replay, test
//! and compare programmable harnesses offline.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

/// The tools a harness run is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ToolsetArg {
    /// Fixture tools only (`--tool-fixture` or the case's `tools`).
    None,
}

/// `sven harness` subcommands; the exit codes are on `sven harness --help`.
#[derive(Subcommand, Debug)]
pub enum HarnessCommands {
    /// Compile harnesses and their children without running them.
    ///
    /// PATH is a harness file, or a directory holding `harness.yaml` or
    /// `bundle.yaml` (plus optional `children/*.yaml`).
    Validate {
        /// Harness files or directories.
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Print the reports as one JSON array on stdout.
        #[arg(long)]
        json: bool,
    },

    /// Show what a harness is: steps, capabilities, limits, digests,
    /// helpers, children and cases.
    Inspect {
        /// A harness file or directory.
        path: PathBuf,
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
    /// Progress lines `[sven:harness] {...}` go to stderr after the run
    /// (one per trace record, derived from the run's trace) unless --quiet.
    /// --json prints one JSON document on stdout and nothing else.
    ///
    /// A real model needs the explicit opt-in on the command line:
    /// `--model <spec>` and `--model-endpoint <url>` of a server you already
    /// run. `--model` without the endpoint is refused (exit 2). sven never
    /// starts or pulls a model and reads no configuration or environment
    /// here.
    Run {
        /// A harness file or directory.
        path: PathBuf,
        #[command(flatten)]
        run: HarnessRunArgs,
    },

    /// Resume a suspended run from its checkpoint.
    ///
    /// Takes the same fixture, model, bound and output flags as `run`; the
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

    /// Run the cases of harness directories on mock models and fixture
    /// tools. Cases naming a live model are skipped unless
    /// `--model-endpoint` names a server you already run.
    Test {
        /// Harness directories (with `cases/*.yaml`).
        #[arg(required = true, value_name = "DIR")]
        dirs: Vec<PathBuf>,
        /// Run only the case with this name.
        #[arg(long, value_name = "NAME")]
        case: Option<String>,
        /// Do not replay each recorded run offline.
        #[arg(long)]
        no_replay: bool,
        /// Print the reports as one JSON array on stdout.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        live: LiveModelArgs,
    },

    /// Run one case set over two harness variants and report the difference.
    Compare {
        /// Variant A (the baseline).
        dir_a: PathBuf,
        /// Variant B.
        dir_b: PathBuf,
        /// The directory of case manifests both variants run.
        #[arg(long, value_name = "DIR")]
        cases: PathBuf,
        /// Exit 1 when a case passes in A and not in B (with `--seed`: when
        /// the paired verdict is `reject`).
        #[arg(long)]
        fail_on_regression: bool,
        /// Run every case under this seed in both variants and compare the
        /// matched outcomes with intervals and cost (repeatable; at least
        /// two make an interval meaningful).
        #[arg(long = "seed", value_name = "N", requires = "bootstrap_seed")]
        seeds: Vec<u64>,
        /// Seeds the interval's bootstrap; the same value reproduces it.
        #[arg(long, value_name = "N", requires = "seeds")]
        bootstrap_seed: Option<u64>,
        /// Print the report as JSON on stdout.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        live: LiveModelArgs,
    },
}

/// The opt-in for cases that name a live model (`test`, `compare`). Without
/// `--model-endpoint` those cases are skipped.
#[derive(Args, Debug)]
pub struct LiveModelArgs {
    /// Replace the model spec (`provider/name`) live cases name.
    #[arg(long, value_name = "SPEC", requires = "model_endpoint")]
    pub model: Option<String>,
    /// Base URL of the model server you already run; the only way live
    /// cases run. sven never starts or pulls a model.
    #[arg(long, value_name = "URL")]
    pub model_endpoint: Option<String>,
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
    /// A case manifest supplying input, model, tools, workspace and bounds.
    /// The flags below override what it gives.
    #[arg(long, value_name = "FILE")]
    pub case: Option<PathBuf>,
    /// A fixture model block (`sequence`, `rules`, `fail`) as a YAML file.
    #[arg(long, value_name = "FILE", conflicts_with = "model")]
    pub model_fixture: Option<PathBuf>,
    /// Fixture tools as a YAML map of tool name to fixture (the `tools:`
    /// block of a case). Repeatable; later files win per tool.
    #[arg(long, value_name = "FILE")]
    pub tool_fixture: Vec<PathBuf>,
    /// An existing directory used as the project root (default: a fresh
    /// empty tempdir, or the case's `workspace` files).
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
    /// A real model, `provider/name`. Needs `--model-endpoint`, as described
    /// in `sven harness run --help`; otherwise exit 2.
    #[arg(long, value_name = "SPEC")]
    pub model: Option<String>,
    /// Base URL of the model server you already run.
    #[arg(long, value_name = "URL", requires = "model")]
    pub model_endpoint: Option<String>,
    /// Tools besides the fixtures. Only `none` is supported: the offline
    /// kit cannot give a run the coding or research tools safely.
    #[arg(long, value_enum, default_value = "none")]
    pub toolset: ToolsetArg,
    /// The run's seed: selects the behaviour of a `by_seed` fixture model.
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,
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
