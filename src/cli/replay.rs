// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The arguments of `sven replay`.
use std::path::PathBuf;

use clap::Args;

/// `sven replay`: which recorded request to send again, how often, and with
/// which seed.
#[derive(Args, Debug)]
pub struct ReplayArgs {
    /// The request log (as written by `--record-requests`)
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// List the recorded requests instead of replaying one
    #[arg(long)]
    pub list: bool,
    /// Which request to replay, as numbered by `--list` (default: the last)
    #[arg(long, value_name = "N")]
    pub request: Option<usize>,
    /// How many times to send it
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    pub repeat: u32,
    /// Sample with this seed
    #[arg(long, conflicts_with = "reported_seed")]
    pub seed: Option<u64>,
    /// Sample with the seed the server reported for the recorded response
    #[arg(long)]
    pub reported_seed: bool,
    /// One JSON object per attempt
    #[arg(long)]
    pub json: bool,
}
