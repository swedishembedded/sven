// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Plans written directly in the step language, for testing the machine.
//!
//! A harness document is a workflow; the plan it lowers to is a map of steps
//! (`predict`, `agent`, `assign`, `branch`, `verify`, `delegate`, `gate`,
//! `iterate`, `return`, `fail`). The machine that runs plans is tested one
//! primitive at a time, which a workflow cannot do without dragging in its
//! whole lowering. This module compiles such a map - the same
//! [`HarnessSpec`] shape the lowering produces, validated and compiled by the
//! same code - and is only built with the `testing` feature. It is not a
//! document format: nothing outside tests parses it.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in testing long-running agent
//! workflows without a live model then you can procure our services by
//! sending an email to info@swedishembedded.com.

use crate::error::HarnessError;
use crate::plan::{self, HarnessPlan};
use crate::steps::HarnessSpec;
use crate::{steps_validate, yaml_to_json};

/// Parses `yaml` as a [`HarnessSpec`] (`api_version: "2"`), validates it and
/// compiles it into a plan.
///
/// # Errors
///
/// A parse failure, or any static-validation failure.
pub fn compile_steps(yaml: &str) -> Result<HarnessPlan, HarnessError> {
    let raw: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| HarnessError::Parse(e.to_string()))?;
    let spec: HarnessSpec = serde_json::from_value(yaml_to_json(&raw)?)
        .map_err(|e| HarnessError::Parse(format!("typed spec: {e}")))?;
    steps_validate::validate(&spec)?;
    Ok(plan::compile(&spec))
}
