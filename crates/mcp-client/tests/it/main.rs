// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it. `conformance` keeps its own binary, since it is
//! also the stdio servers it tests.

mod account_connect;
mod files;
mod observers;
mod served_skills;
mod sign_in;
mod slow_startup;
