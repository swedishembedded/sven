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
//! let reply = agent.send("summarise the build failure").await?.reply;
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
mod run;
mod state;
mod transcript;

pub use agent::Agent;
/// The trajectory interchange format [`Agent::trajectory`] exports.
pub use atif;
pub use engine::{ApprovalPolicy, Engine, EngineBuilder, Toolset};
pub use error::CallError;
pub use method::{Method, Strategy};
pub use run::{CancelToken, Question, RunConclusion, RunOptions, RunOutcome, Usage};
/// The schema derive a [`Method`]'s return type needs (`#[derive(JsonSchema)]`),
/// at the version this SDK uses.
pub use schemars;
pub use state::AgentState;
pub use sven_bootstrap::session_handles::HumanGate;
pub use transcript::{ToolCallRecord, Turn};

/// Sven's configuration - which model, which provider, which limits.
///
/// [`EngineBuilder::config`] takes a [`Config`](config::Config), so an
/// application that wants any model other than the compiled-in default has to
/// be able to name that type and, usually, to load it the way the CLI does.
/// Both live here so reaching a model never requires depending on a crate
/// behind the facade.
///
/// [`load`](config::load) reads the same files and environment the `sven`
/// binary reads, including its detection of a locally served model, so an
/// application configured once works for both.
pub mod config {
    pub use sven_config::{load, Config};
}

/// Everything needed to give an agent a tool of your own.
///
/// Implement [`Tool`](tool::Tool) and register it with
/// [`EngineBuilder::tool`]. Re-exported here so an application never has to
/// name a kernel crate to extend the agent.
pub mod tool {
    pub use sven_tool_api::{
        ApprovalPolicy, OutputCategory, Tool, ToolCall, ToolCapability, ToolDisplay, ToolOutput,
        ToolOutputPart, NO_USER_ANSWER,
    };
}

/// Everything needed to run a state machine of your own.
///
/// Implement [`Machine`](machine::Machine) and register it with
/// [`EngineBuilder::machine`]. The kernel drives it exactly as it drives the
/// built-in ones, including permissions, audit and suspend/resume.
pub mod machine {
    pub use sven_hsm::{
        Context, Effect, ErasedMachine, Event, GatedCall, Hsm, Machine, MachineId, Reaction,
        ToolCapability,
    };
    pub use sven_machines::mode::MachineFactory;
    pub use sven_machines::ModeRegistry;
}

/// Declares an agent from a Rust trait: its documentation is its prompt.
///
/// See the [`agent`] macro's own documentation for the rules.
pub use sven_sdk_macros::agent;

/// The event stream an agent emits while it works.
///
/// Re-exported so a consumer never needs to name a kernel crate to render
/// progress; it is the same value the TUI and the headless runner consume.
pub use sven_vocab::SessionEvent;

/// Everything needed to back the agent with a model of your own.
///
/// Implement [`model::ModelProvider`] and hand it to
/// [`EngineBuilder::model_provider`](crate::EngineBuilder::model_provider) -
/// the same seam the built-in OpenAI/Anthropic/OpenRouter providers hang
/// off, so an in-process model (a local inference engine, a recorder, a
/// test double) is a peer of the remote ones, not a special case.
///
/// Re-exported here so an application never has to name a kernel crate to
/// serve the agent's model.
pub mod model {
    pub use sven_model::{
        CompletionRequest, ContentPart, FunctionCall, Message, MessageContent, ModelProvider,
        ResponseEvent, ResponseFormat, ResponseStream, Role, ToolContentPart, ToolResultContent,
        ToolSchema,
    };
}

/// The built-in model providers, selected from [`config::Config`].
///
/// [`from_config`](drivers::from_config) constructs whichever provider the
/// configuration names - OpenAI, Anthropic, OpenRouter, a brain-served local
/// model, the mock - already clamped to the configured context window and
/// output limit, ready for
/// [`EngineBuilder::model_provider`](crate::EngineBuilder::model_provider).
/// [`from_config_probed`](drivers::from_config_probed) additionally probes a
/// live server for its real context window before clamping.
///
/// The [`model`](crate::model) module is the seam; this is what the shipped
/// drivers hanging off it look like. Re-exported so an application that is
/// content with the built-ins never has to reach past the facade.
pub mod drivers {
    pub use sven_model_drivers::{from_config, from_config_probed};
}
