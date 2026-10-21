// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One command of a gate.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning build and test
//! scripts into trusted checks then you can procure our services by sending
//! an email to info@swedishembedded.com.

use serde::{Deserialize, Serialize};

/// A gate runs commands in order; each has a name its result reports it under
/// (`failed_step`) and an argv.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateCommand {
    /// The step's name in the result.
    pub name: String,
    /// The argv; each element may be a template over run data.
    pub argv: Vec<String>,
}
