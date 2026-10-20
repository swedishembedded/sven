// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness`: the offline CLI over programmable harnesses.
//!
//! Every subcommand is a thin shell over `sven-harness-testkit`, which builds
//! each run on a fresh engine with an injected provider and
//! `Config::default()`. Nothing here loads `Settings` or any user
//! configuration, and a real model is built only under the testkit's explicit
//! opt-in, so no subcommand can start or contact a model by default.
//!
//! Progress lines (`[sven:harness] ...`) are derived from the run's trace
//! after it ends: the testkit does not expose the live event stream.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod exec;
mod helper;
mod inspect;
mod production;
mod replay;
mod request;
mod suites;
mod views;

use sven_harness_testkit::TestkitError;

use crate::cli::HarnessCommands;

/// A command that could not produce its result: the exit code and why.
pub(super) struct Failure {
    pub code: i32,
    pub message: String,
}

pub(super) type Outcome = Result<i32, Failure>;

/// A usage error or an unusable file or harness: exit 2.
pub(super) fn usage(message: impl Into<String>) -> Failure {
    Failure {
        code: 2,
        message: message.into(),
    }
}

/// Maps a testkit error to its exit code: a run that could not be carried
/// out is infrastructure (5), everything else is the caller's input (2).
pub(super) fn kit(error: TestkitError) -> Failure {
    let code = if matches!(error, TestkitError::Run(_)) {
        5
    } else {
        2
    };
    Failure {
        code,
        message: error.to_string(),
    }
}

/// Runs a `sven harness` subcommand and returns the process exit code.
pub(crate) async fn run_harness_command(command: &HarnessCommands) -> i32 {
    let outcome = match command {
        HarnessCommands::Validate { paths, json } => suites::validate(paths, *json),
        HarnessCommands::Inspect { path, json, graph } => match graph {
            Some(format) => views::graph(path, *format),
            None => inspect::inspect(path, *json),
        },
        HarnessCommands::Compile {
            path,
            format,
            output,
        } => views::compile(path, *format, output.as_deref()),
        HarnessCommands::Explain { path, step } => views::explain(path, step.as_deref()),
        HarnessCommands::Status {
            path,
            checkpoint,
            json,
        } => views::status(path, checkpoint, *json),
        HarnessCommands::Schema { path } => inspect::schema(path.as_deref()),
        HarnessCommands::Bundle {
            document,
            helpers,
            output,
        } => helper::bundle(document, helpers, output.as_deref()),
        HarnessCommands::Helper { command } => helper::run_helper(command),
        HarnessCommands::Run { path, run } => exec::run(path, run).await,
        HarnessCommands::Resume {
            path,
            checkpoint,
            run,
        } => exec::resume(path, checkpoint, run).await,
        HarnessCommands::Replay {
            path,
            recording,
            expect_trace,
            json,
        } => replay::replay(path, recording, expect_trace.as_deref(), *json),
        HarnessCommands::Test {
            dirs,
            case,
            no_replay,
            json,
            live,
        } => {
            suites::test(
                dirs,
                case.as_deref(),
                !*no_replay,
                *json,
                request::live_opt_in(live),
            )
            .await
        }
        HarnessCommands::Compare {
            dir_a,
            dir_b,
            cases,
            fail_on_regression,
            seeds,
            bootstrap_seed,
            json,
            live,
        } => {
            let opt_in = request::live_opt_in(live);
            let compare = suites::CompareArgs {
                dir_a,
                dir_b,
                cases,
                fail_on_regression: *fail_on_regression,
                paired_seeds: bootstrap_seed.map(|bootstrap| (seeds.clone(), bootstrap)),
                json: *json,
            };
            suites::compare(compare, opt_in).await
        }
    };
    outcome.unwrap_or_else(|failure| {
        eprintln!("error: {}", failure.message);
        failure.code
    })
}

/// Prints `value` as pretty JSON on stdout.
pub(super) fn print_json(value: &impl serde::Serialize) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(value).map_err(|e| Failure {
        code: 5,
        message: format!("cannot serialize the report: {e}"),
    })?;
    println!("{text}");
    Ok(())
}
