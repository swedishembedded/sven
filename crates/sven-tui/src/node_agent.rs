// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Node-proxy agent backend — re-exported from `sven-frontend` for TUI use.
//!
//! The canonical implementation lives in `sven_frontend::node_agent`. This
//! module re-exports everything needed by the TUI so that all internal
//! references to `crate::node_agent::node_agent_task` and
//! `crate::node_agent::fetch_node_tools` continue to resolve without changes.

pub use sven_frontend::node_agent::{fetch_node_tools, node_agent_task};
