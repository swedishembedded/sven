// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reading the flags `run` and `resume` share: the run's inputs, its
//! scratchpad and its bounds.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};

use super::{usage, Failure};
use crate::cli::HarnessRunArgs;

/// The `--scratchpad` directory as an absolute path, so that the run does not
/// depend on where the process happens to be.
pub(super) fn scratchpad(args: &HarnessRunArgs) -> Result<Option<PathBuf>, Failure> {
    args.scratchpad
        .as_deref()
        .map(|dir| {
            std::path::absolute(dir)
                .map_err(|e| usage(format!("--scratchpad {}: {e}", dir.display())))
        })
        .transpose()
}

/// The input the flags give (`--input` or `--input-file`), or an empty
/// object.
pub(super) fn input(args: &HarnessRunArgs) -> Result<serde_json::Value, Failure> {
    match (&args.input, &args.input_file) {
        (Some(text), _) => parse_json(text, "--input"),
        (None, Some(file)) => parse_json(&read(file)?, &file.display().to_string()),
        (None, None) => Ok(serde_json::json!({})),
    }
}

/// The deadline `--timeout` gives, in milliseconds.
pub(super) fn deadline_ms(args: &HarnessRunArgs) -> Result<Option<u64>, Failure> {
    args.timeout
        .map(|secs| {
            secs.checked_mul(1000)
                .ok_or_else(|| usage("--timeout is too large"))
        })
        .transpose()
}

pub(super) fn read(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path).map_err(|e| usage(format!("cannot read {}: {e}", path.display())))
}

pub(super) fn parse_json(text: &str, origin: &str) -> Result<serde_json::Value, Failure> {
    serde_json::from_str(text).map_err(|e| usage(format!("{origin} is not valid JSON: {e}")))
}
