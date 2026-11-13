// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it.

mod answer;
mod matrix;
mod mcp_stub;
mod plan;
mod provider_server;
mod providers;
mod real_report;
mod report;
mod score;
mod segments;
mod stats;
mod suites;
