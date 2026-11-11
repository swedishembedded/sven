// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary so the fixtures in
//! [`common`] are shared rather than compiled (and half unused) per file.

mod batching;
mod common;
mod isolation;
mod pictures;
mod pipeline;
