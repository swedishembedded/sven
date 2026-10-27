// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The arguments of `sven rc`.
use std::path::PathBuf;

use clap::Args;

/// `sven rc`: where the remote sessions work.
#[derive(Args, Debug)]
pub struct RcArgs {
    /// Where the sessions work; the current directory by default.
    #[arg(long)]
    pub working_dir: Option<PathBuf>,
}
