// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Loading sven's configuration file.
//!
//! Where the files are found and in which order they override each other,
//! how `${VAR}` placeholders expand, how the layers merge into one document,
//! and which of its keys no section recognises. What a section holds - its
//! types, defaults and validation - belongs to the crate that reads it: the
//! model provider settings to `sven-model-drivers`, the runtime and tool
//! settings to `sven-bootstrap` and each tool's own crate, the MCP servers to
//! `sven-mcp-client`, the interactive UI to `sven-frontend`. This crate knows
//! none of them.

mod document;
mod schema;

pub use document::ConfigDocument;
pub use schema::Schema;

/// Serde default helper - returns `true`.
///
/// For a `bool` setting that is on unless the file turns it off:
/// `#[serde(default)]` on a `bool` falls back to `false`, so a named function
/// is required (`#[serde(default = "sven_config::default_true")]`).
#[must_use]
pub fn default_true() -> bool {
    true
}
