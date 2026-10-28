// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for operating coding agents from
// anywhere while they work on the machine that has the code, for its
// clients. If your team needs expertise in remote agent control then you can
// procure our services by sending an email to info@swedishembedded.com.
//! Remote control of this machine's agent sessions through a server.
//!
//! * [`login`] - the one server this user signed in to, and its token
//!   (`sven auth login --server URL`).
//! * [`client`] - the HTTP binding of [`sven_control::remote`]: discovery,
//!   whoami, the command stream and event uploads.
//! * [`host`] - `sven rc`: one process that runs any number of sessions for
//!   the server, each an actor owning its own session controller.
//! * [`attach`] - `sven rc attach --tui`: a conversation the server keeps, as
//!   the agent session the full-screen UI shows.
//! * [`sandbox`] - `sven rc --hosted`: the kernel confines a hosted host to
//!   its own directory.
//! * [`transcript`] - `sven rc show` and `sven rc attach`: a conversation the
//!   server keeps, synced as a replica of its shared document and read (and,
//!   attached, written to through the server) in the terminal.
//!
//! The server never reaches into the machine. The host connects out, takes
//! commands, and runs them through the same session controller every other
//! surface uses, under the same permission policy.

pub mod approvals;
pub mod attach;
pub mod client;
pub mod container;
pub mod host;
pub mod login;
pub mod sandbox;
pub mod sessions;
mod sse;
pub mod transcript;

pub use client::{
    normalize_server, Busy, ConversationNotFound, Disconnected, EnvironmentTaken, RemoteClient,
};
pub use host::{restrict_to_hosted, run_host, HostOptions, HOSTED_TOOLS};
pub use login::RemoteLogin;
pub use sessions::SessionStore;
