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
mod events;
pub mod fork;
mod gates;
mod harness;
mod memory_scope;
mod meter;
mod method;
mod run;
mod run_support;
pub mod search;
mod state;
mod transcript;

pub use agent::Agent;
/// The trajectory interchange format [`Agent::trajectory`] exports.
pub use atif;
pub use engine::{ApprovalPolicy, Engine, EngineBuilder, Toolset};
pub use error::CallError;
pub use events::{EventStream, RunEvent};
pub use gates::HumanGate;
pub use harness::{
    compile_harness, Correction, Harness, HarnessCheckpoint, HarnessFailure, HarnessOutcome,
    HarnessRecording, HarnessReplay, HarnessRun, RefusedMessage,
};
pub use memory_scope::MemoryScope;
pub use method::{Method, Strategy};
pub use run::{
    Approval, CancelToken, Decision, Question, RunConclusion, RunId, RunOptions, RunOutcome, Usage,
};
/// The schema derive a [`Method`]'s return type needs (`#[derive(JsonSchema)]`),
/// at the version this SDK uses.
pub use schemars;
pub use state::AgentState;
pub use sven_harness_runtime::{sourced_trace, Source, SourceTracker, SourcedRecord, TraceRecord};
/// Identifies an approval, as a [`HumanGate::Approval`] and a parked
/// [`Approval`] carry it.
pub use sven_hsm::ApprovalId;
pub use sven_vocab::HarnessEvent;
pub use transcript::{ToolCallRecord, Turn};

/// The compiled form of a harness: what [`Harness::plan`] returns, so a
/// caller can name its steps, limits and declared types without depending
/// on a crate behind the facade.
pub mod harness_plan {
    pub use sven_harness_spec::{HarnessPlan, IOType, PlanLimits, PlanPrimitive, PlanStep};
}

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
    pub use sven_bootstrap::Config;

    /// Loads the configuration the `sven` binary reads: [`Config::load`].
    ///
    /// # Errors
    ///
    /// As [`Config::load`].
    pub fn load(extra: Option<&std::path::Path>) -> anyhow::Result<Config> {
        Config::load(extra)
    }
}

/// Everything needed to give an agent a tool of your own.
///
/// Implement [`Tool`](tool::Tool) and register it with
/// [`EngineBuilder::tool`]. Re-exported here so an application never has to
/// name a kernel crate to extend the agent.
pub mod tool {
    pub use sven_tool_api::call_records::record_memory_retrieval;
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
        Context, Effect, ErasedMachine, Event, GatedCall, Hsm, Machine, MachineId,
        PermissionPolicy, Reaction, ToolCapability,
    };
    pub use sven_machines::mode::MachineFactory;
    pub use sven_machines::ModeRegistry;
}

/// Declares an agent from a Rust trait: its documentation is its prompt.
///
/// See the [`agent`] macro's own documentation for the rules.
pub use sven_sdk_macros::agent;

/// A hash-chained, append-only JSONL log, the format sven's own audit trail
/// and ledgers are written in.
///
/// Re-exported so an application that records what its agents did can keep
/// its records in the same tamper-evident, crash-safe and multi-writer-safe
/// form instead of writing a second log format.
pub use sven_chain as chain;

/// The definitions that shape an agent in a project: Markdown subagents,
/// skills and reusable commands, found by the same rules the interactive
/// sessions use (a project's own definitions override those of its parent
/// directories and of the user's home).
///
/// Re-exported so an application that runs an agent in a project can report
/// exactly which definitions were in effect, without a second discovery
/// implementation that could disagree with sven's.
pub mod workspace {
    pub use sven_workspace::{
        discover_agents, discover_commands, discover_skills, find_project_context_file, AgentInfo,
        SkillInfo,
    };
}

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
    /// Whether a call repeats an earlier one of the run: [`repeat::call_identity`]
    /// (the call without cosmetic differences) and
    /// [`repeat::earlier_identical_results`]. A caller that bounds repetition
    /// from outside compares calls the same way the agent's own notes do.
    pub use sven_model::repeat;
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
/// The [`model`] module is the seam; this is what the shipped
/// drivers hanging off it look like. Re-exported so an application that is
/// content with the built-ins never has to reach past the facade.
pub mod drivers {
    pub use sven_model_drivers::{from_config, from_config_probed};
}

/// Agent memory: the store contract an embedding application implements or
/// borrows, the bundled embedders, and the tool an agent reaches memory through.
/// Give an agent a scoped store with [`Agent::with_memory`].
pub mod memory {
    pub use sven_memory::{HashingEmbedder, SemanticMemoryTool, NOTES_SEGMENTER, PERSONAL_SPACE};
    pub use sven_memory_api::*;
    #[cfg(feature = "sqlite-memory")]
    pub use sven_memory_sqlite::SqliteMemory;
}

/// Deterministic segmentation of sources into memory units, anchors that
/// survive edits, redaction, BM25 and reciprocal rank fusion.
pub use sven_sources as sources;
