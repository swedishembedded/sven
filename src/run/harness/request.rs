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
use sven_harness_testkit::{CaseManifest, ModelFixture, OptIn, RunRequest, ToolFixture, Workspace};

use super::{usage, Failure};
use crate::cli::{HarnessRunArgs, LiveModelArgs, ToolsetArg};

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
pub(super) fn build(args: &HarnessRunArgs) -> Result<RunRequest, Failure> {
    // Fixture tools are the only tools an offline run is given.
    match args.toolset {
        ToolsetArg::None => {}
    }
    let case = args
        .case
        .as_deref()
        .map(CaseManifest::from_path)
        .transpose()
        .map_err(super::kit)?;

    let input = match (&args.input, &args.input_file) {
        (Some(text), _) => parse_json(text, "--input")?,
        (None, Some(file)) => parse_json(&read(file)?, &file.display().to_string())?,
        (None, None) => case
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

    let mut bounds = case.map(|c| c.bounds).unwrap_or_default();
    if let Some(secs) = args.timeout {
        bounds.deadline_ms = Some(
            secs.checked_mul(1000)
                .ok_or_else(|| usage("--timeout is too large"))?,
        );
    }
    if let Some(tokens) = args.max_output_tokens {
        bounds.max_output_tokens = Some(tokens);
    }

    Ok(RunRequest {
        input,
        model,
        tools,
        workspace,
        bounds,
        opt_in: opt_in(args),
        record: args.record.is_some(),
        trajectory: args.trajectory.is_some(),
    })
}

fn read(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path).map_err(|e| usage(format!("cannot read {}: {e}", path.display())))
}

fn parse_json(text: &str, origin: &str) -> Result<serde_json::Value, Failure> {
    serde_json::from_str(text).map_err(|e| usage(format!("{origin} is not valid JSON: {e}")))
}

fn parse_yaml<T: DeserializeOwned>(path: &Path) -> Result<T, Failure> {
    serde_yaml::from_str(&read(path)?)
        .map_err(|e| usage(format!("{}: invalid fixture: {e}", path.display())))
}
