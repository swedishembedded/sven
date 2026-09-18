// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The public Sven framework.
//!
//! Sven's kernel is a deterministic hierarchical state machine that treats the
//! model as an untrusted reasoning service. This crate is the front door to it:
//! the surface an application builds on, of which the `sven` CLI is one
//! consumer among several.
//!
//! Three layers, with deliberately different lifetimes:
//!
//! | Layer | Owns | Lives for |
//! |-------|------|-----------|
//! | [`Engine`] | Model clients, tool registry, configuration | The process |
//! | [`Agent`] | One conversation's history and machine state | A task |
//! | [`AgentState`] | The same, serialized, with no live handles | Storage |
//!
//! The split is what lets a service hold one engine and serve many agents:
//! the expensive resources are built once, while an agent is cheap enough to
//! load, advance by a single step, persist and drop between requests.
//!
//! ```no_run
//! # async fn f() -> anyhow::Result<()> {
//! use sven_sdk::Engine;
//!
//! let engine = Engine::builder().build()?;
//!
//! let mut agent = engine.agent("agent");
//! let reply = agent.send("summarise the build failure").await?;
//!
//! // Put it down; pick it up later, in another process, on another engine.
//! let stored = serde_json::to_string(&agent.suspend())?;
//! let mut resumed = engine.resume(serde_json::from_str(&stored)?)?;
//! resumed.send("now propose a fix").await?;
//! # Ok(())
//! # }
//! ```
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients. If
//! your team needs expertise in building services on top of agent state
//! machines then you can procure our services by sending an email to
//! info@swedishembedded.com.

#![warn(missing_docs)]

mod agent;
mod engine;
mod error;
mod method;
mod state;

pub use agent::Agent;
pub use engine::{ApprovalPolicy, Engine, EngineBuilder};
pub use error::CallError;
pub use method::{Method, Strategy};
pub use state::AgentState;

/// The event stream an agent emits while it works.
///
/// Re-exported so a consumer never needs to name a kernel crate to render
/// progress; it is the same value the TUI and the headless runner consume.
pub use sven_vocab::SessionEvent;
