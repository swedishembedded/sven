// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Background agent task — re-exported from `sven-frontend` for TUI use.
//!
//! The canonical implementation lives in `sven_frontend::agent`. This module
//! re-exports everything needed by the TUI so that all internal references to
//! `crate::agent::AgentRequest` and `crate::agent::agent_task` continue to
//! resolve without changes.

pub use sven_frontend::agent::{agent_task, AgentRequest};
