// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Shell execution tool.
//!
//! `tool` is the tool itself; `capture` runs the command and streams its
//! output into an output buffer; `render` decides what of that output the
//! model sees; `kill_group` ends a command's whole process group.

mod capture;
#[cfg(unix)]
pub(crate) mod kill_group;
mod render;
mod tool;

#[cfg(test)]
mod bounded_tests;
#[cfg(test)]
mod tests;

pub use tool::{ShellTool, DEFAULT_CAPTURE_QUOTA_BYTES, DEFAULT_MAX_TIMEOUT_SECS};
