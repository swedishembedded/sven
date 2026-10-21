// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness`: validate, inspect, compile, explain, run, resume and
//! replay programmable harnesses.
//!
//! The reading commands (`validate`, `inspect`, `compile`, `explain`,
//! `status`, `schema`) compile a harness and look at it; none starts a model.
//! `run` and `resume` build one engine from the flags (a model server you
//! already run, the coding tools on a workspace you name, an approval mode you
//! choose), load the harness onto it and drive the run, printing
//! `[sven:harness] ...` progress lines as it goes. `replay` re-derives a
//! recorded run offline. Nothing here loads `Settings` or any user
//! configuration.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod args;
mod engine;
mod engine_run;
mod error;
mod exec;
mod inspect;
mod plan_view;
mod render;
mod replay;
mod source;
mod stamps;
mod status;
mod views;

use error::HarnessError;

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

/// Maps an error to its exit code: a run that could not be carried out is
/// infrastructure (5), everything else is the caller's input (2).
fn kit(error: HarnessError) -> Failure {
    let code = if matches!(error, HarnessError::Run(_)) {
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
        HarnessCommands::Validate { paths, json } => inspect::validate(paths, *json),
        HarnessCommands::Inspect { path, json, graph } => match graph {
            Some(format) => views::graph(path, *format),
            None => inspect::inspect(path, *json),
        },
        HarnessCommands::Compile {
            path,
            format,
            output,
        } => views::compile(path, *format, output.as_deref()),
        HarnessCommands::Explain {
            path,
            step,
            input,
            input_file,
            elide,
        } => views::explain(
            path,
            step.as_deref(),
            views::ExplainInputs {
                json: input.as_deref(),
                file: input_file.as_deref(),
                elide: *elide,
            },
        ),
        HarnessCommands::Status {
            path,
            checkpoint,
            json,
        } => views::status(path, checkpoint, *json),
        HarnessCommands::Schema { path } => inspect::schema(path.as_deref()),
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
