// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `atif` — an independent Rust implementation of the **Agent Trajectory
//! Interchange Format (ATIF) v1.7** wire schema, a validator, and a small
//! set of persistence helpers (atomic whole-document writes, cheap
//! header-only reads, and NDJSON step streaming).
//!
//! This crate is a standalone, spec-complete building block with zero
//! dependencies on other sven crates; `sven-session-store::trace_session`
//! builds the session store backing the TUI and CI surfaces on it, and
//! `sven-sdk` re-exports it for `Agent::trajectory`. See the ATIF
//! RFC (v1.7) for the normative schema this crate mirrors byte-for-byte on
//! the wire, even though the Rust-side type and module names here are an
//! independent design.
//!
//! # Module map
//!
//! - [`model`] — the ATIF schema: [`Trajectory`], steps, tool calls,
//!   observations, multimodal content, subagent references, and the
//!   Section VII context-management convention.
//! - [`validate`] — [`validate::validate_trajectory`], which walks a
//!   [`Trajectory`] (recursively, through embedded subagents) and collects
//!   every rule violation rather than stopping at the first one.
//! - [`persist`] — atomic whole-document JSON writes with concurrent
//!   modification detection, a fast header-only reader, and an NDJSON
//!   step stream reader/writer.

pub mod model;
pub mod persist;
pub mod validate;

pub use model::*;
pub use validate::{validate_trajectory, ValidationError};
