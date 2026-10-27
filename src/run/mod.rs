// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One handler module per `sven` subcommand group (see `src/cli/` for the
//! matching clap grammar). `main.rs` parses [`crate::cli::Cli`] and dispatches
//! into these.

pub(crate) mod agent;
pub(crate) mod agent_dispatch;
pub(crate) mod auth;
pub(crate) mod chats;
pub(crate) mod ci;
pub(crate) mod config;
pub(crate) mod decisions;
#[cfg(feature = "document")]
pub(crate) mod document;
pub(crate) mod harness;
pub(crate) mod index;
pub(crate) mod ledger;
pub(crate) mod logging;
pub(crate) mod models;
pub(crate) mod oauth;
pub(crate) mod pipeline;
pub(crate) mod provider_doctor;
pub(crate) mod questions;
pub(crate) mod replay;
pub(crate) mod secret;
pub(crate) mod task;
#[cfg(feature = "network")]
pub(crate) mod team;
pub(crate) mod tool;
#[cfg(feature = "tui")]
pub(crate) mod tui;
pub(crate) mod workflow;
