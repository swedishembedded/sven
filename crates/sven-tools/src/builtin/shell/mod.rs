// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! Shell execution tool.

mod tool;
pub(crate) use tool::head_tail_truncate;
pub use tool::ShellTool;
