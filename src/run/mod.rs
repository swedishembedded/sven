// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One handler module per `sven` subcommand group (see `src/cli/` for the
//! matching clap grammar). `main.rs` parses [`crate::cli::Cli`] and dispatches
//! into these.

pub(crate) mod acp;
pub(crate) mod chats;
pub(crate) mod ci;
pub(crate) mod cloud;
pub(crate) mod index;
pub(crate) mod logging;
pub(crate) mod mcp;
pub(crate) mod models;
pub(crate) mod node;
pub(crate) mod oauth;
pub(crate) mod peer;
pub(crate) mod pipeline;
pub(crate) mod share;
pub(crate) mod team;
pub(crate) mod tool;
pub(crate) mod tui;
pub(crate) mod workflow;
