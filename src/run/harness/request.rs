// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Turning `run`/`resume` flags (and an optional case manifest) into the
//! testkit's `RunRequest`. Flags override the manifest.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::DeserializeOwned;
use sven_harness_testkit::{
    Bounds, CaseManifest, ModelFixture, OptIn, RunRequest, ToolFixture, TraceObserver, Workspace,
};

use super::{usage, Failure};
use crate::cli::{HarnessRunArgs, LiveModelArgs};

/// What a run uses when nothing names a model: every model call fails with
/// this message.
const NO_MODEL: &str = "no model source: pass --model-fixture or --model";

/// The opt-in a live model needs: `--model-endpoint`, and nothing else.
fn opt_in_from(model: Option<&str>, endpoint: Option<&str>) -> OptIn {
    endpoint.map_or_else(OptIn::none, |endpoint| OptIn::live(model, endpoint))
}

/// The opt-in of `run` and `resume`.
pub(super) fn opt_in(args: &HarnessRunArgs) -> OptIn {
    opt_in_from(args.model.as_deref(), args.model_endpoint.as_deref())
}

/// The opt-in of `test` and `compare`.
pub(super) fn live_opt_in(args: &LiveModelArgs) -> OptIn {
    opt_in_from(args.model.as_deref(), args.model_endpoint.as_deref())
}

/// Whether a request's model is the default one that only fails.
pub(super) fn is_default_model(request: &RunRequest) -> bool {
    matches!(&request.model, Some(ModelFixture::Fail(message)) if message == NO_MODEL)
}

/// Builds the request for a run of `args`.
pub(super) fn build(args: &HarnessRunArgs, progress: TraceObserver) -> Result<RunRequest, Failure> {
    // Fixture tools are the only tools an offline run is given; the flags
    // that configure a production run mean nothing here and are refused
    // rather than ignored.
    for (given, flag) in [
        (args.approvals.is_some(), "--approvals"),
        (!args.allow_capabilities.is_empty(), "--allow-capability"),
        (args.api_key_file.is_some(), "--api-key-file"),
        (args.first_byte_timeout.is_some(), "--first-byte-timeout"),
    ] {
        if given {
            return Err(usage(format!(
                "{flag} configures a production run: it needs --toolset coding"
            )));
        }
    }
    let case = args
        .case
        .as_deref()
        .map(CaseManifest::from_path)
        .transpose()
        .map_err(super::kit)?;

    let input = match explicit_input(args)? {
        Some(input) => input,
        None => case
            .as_ref()
            .map_or_else(|| serde_json::json!({}), |c| c.input.clone()),
    };

    let model = if let Some(file) = &args.model_fixture {
        Some(parse_yaml::<ModelFixture>(file)?)
    } else if let Some(spec) = &args.model {
        Some(ModelFixture::Live {
            model: spec.clone(),
        })
    } else {
        case.as_ref()
            .and_then(|c| c.model.clone())
            .or_else(|| Some(ModelFixture::Fail(NO_MODEL.to_string())))
    };

    let mut tools: BTreeMap<String, ToolFixture> =
        case.as_ref().map(|c| c.tools.clone()).unwrap_or_default();
    for file in &args.tool_fixture {
        tools.extend(parse_yaml::<BTreeMap<String, ToolFixture>>(file)?);
    }

    let workspace = match (&args.workspace, &case) {
        (Some(dir), _) => Workspace::Dir(dir.clone()),
        (None, Some(c)) => Workspace::Fresh(c.workspace.clone()),
        (None, None) => Workspace::default(),
    };

    let bounds = bounds(args, case.map(|c| c.bounds).unwrap_or_default())?;

    Ok(RunRequest {
        input,
        model,
        tools,
        workspace,
        bounds,
        opt_in: opt_in(args),
        seed: args.seed,
        record: args.record.is_some(),
        trajectory: args.trajectory.is_some() || args.conversations.is_some(),
        scratchpad: scratchpad(args)?,
        progress: Some(progress),
    })
}

/// The `--scratchpad` directory as an absolute path, so that the run does not
/// depend on where the process happens to be.
pub(super) fn scratchpad(args: &HarnessRunArgs) -> Result<Option<std::path::PathBuf>, Failure> {
    args.scratchpad
        .as_deref()
        .map(|dir| {
            std::path::absolute(dir)
                .map_err(|e| usage(format!("--scratchpad {}: {e}", dir.display())))
        })
        .transpose()
}

/// The input the flags give (`--input` or `--input-file`), if any.
pub(super) fn explicit_input(args: &HarnessRunArgs) -> Result<Option<serde_json::Value>, Failure> {
    match (&args.input, &args.input_file) {
        (Some(text), _) => parse_json(text, "--input").map(Some),
        (None, Some(file)) => parse_json(&read(file)?, &file.display().to_string()).map(Some),
        (None, None) => Ok(None),
    }
}

/// `base` with the flags' deadline and token budget laid over it.
pub(super) fn bounds(args: &HarnessRunArgs, base: Bounds) -> Result<Bounds, Failure> {
    let mut bounds = base;
    if let Some(secs) = args.timeout {
        bounds.deadline_ms = Some(
            secs.checked_mul(1000)
                .ok_or_else(|| usage("--timeout is too large"))?,
        );
    }
    if let Some(tokens) = args.max_output_tokens {
        bounds.max_output_tokens = Some(tokens);
    }
    Ok(bounds)
}

pub(super) fn read(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path).map_err(|e| usage(format!("cannot read {}: {e}", path.display())))
}

pub(super) fn parse_json(text: &str, origin: &str) -> Result<serde_json::Value, Failure> {
    serde_json::from_str(text).map_err(|e| usage(format!("{origin} is not valid JSON: {e}")))
}

fn parse_yaml<T: DeserializeOwned>(path: &Path) -> Result<T, Failure> {
    serde_yaml::from_str(&read(path)?)
        .map_err(|e| usage(format!("{}: invalid fixture: {e}", path.display())))
}
