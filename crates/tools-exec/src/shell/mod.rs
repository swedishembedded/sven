// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Shell execution tool.

mod tool;
#[cfg(unix)]
pub(crate) use tool::kill_group;
pub use tool::{ShellTool, DEFAULT_MAX_TIMEOUT_SECS};
