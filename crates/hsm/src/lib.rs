//! `sven-hsm` - a deterministic Hierarchical State Machine kernel.
//!
//! This crate is the foundation of the Sven runtime. It owns **all control
//! flow**: the LLM is an untrusted reasoning service that only returns typed
//! [`Event`]s, tools run only via typed [`Effect`]s the machine emits, and a
//! single permission choke point makes unsafe tool use unrepresentable.
//!
//! The crate depends only on `sven-vocab` (the pure session-event vocabulary
//! `UiEvent` re-exports), plus `serde`, `serde_json`, `uuid`, `thiserror`, and
//! `tokio` (for `observation`'s broadcast channel only - the crate has no
//! `async-trait` and no active-object execution machinery). Everything else
//! in Sven is built on top of it.
//!
//! # The spine
//!
//! ```text
//! LLM proposes -> HSM decides -> Effect executor acts -> Events record reality
//!              -> Guards enforce safety -> Humans approve irreversible steps
//! ```
//!
//! Every transition function is **pure**: it mutates only the extended
//! [`Context`] and *returns* [`Effect`]s; it performs no I/O. `sven-kernel`
//! (a separate crate, one tier up) executes those effects on separate tasks
//! and feeds results back as [`Event`]s, preserving run-to-completion via a
//! single consumer task.
//!
//! # Architecture
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`ids`] | newtype identifiers wrapping [`uuid::Uuid`] |
//! | [`event`] | the [`Event`] vocabulary + payload-free [`EventKind`] |
//! | [`effect`] | the [`Effect`] vocabulary + [`EffectKind`] |
//! | [`context`] | extended state ([`Context`]) - "what is known" |
//! | [`status`] | [`Reaction`] - what a handler returns |
//! | [`machine`] | the [`Machine`] trait user state machines implement |
//! | [`dispatch`] | the generic HSM engine ([`Hsm`]) - Super walk + LCA + entry/exit + Init |
//! | [`submachine`] | hierarchical composition ([`Submachine`], [`ErasedMachine`]) |
//! | [`permissions`] | [`PermissionPolicy`] + [`validate_effects_are_allowed`] choke point |
//! | [`audit`] | [`AuditRecord`] + event-sourcing [`replay`] |
//! | [`observation`] | the outward broadcast plane ([`ObservationSink`], [`UiEvent`]) |
//! | [`error`] | [`MachineError`] |
//!
//! # Building a machine
//!
//! Implement [`Machine`] for your state enum, then drive it with [`Hsm`] (pure,
//! synchronous, perfect for tests) or `sven_kernel::Runtime`/`ErasedRuntime`
//! (async, with real effect execution, in the separate `sven-kernel` crate).
//! See that crate's integration tests for a worked 3-level example.

#![warn(missing_docs)]

pub mod audit;
pub mod context;
pub mod dispatch;
pub mod effect;
pub mod error;
pub mod event;
pub mod ids;
pub mod machine;
pub mod observation;
pub mod permissions;
pub mod report;
pub mod snapshot;
pub mod status;
pub mod submachine;

// ---- Curated public API re-exports ----

pub use audit::{replay, AuditOutcome, AuditRecord, ToolAuditOutcome, ToolAuditRecord};
pub use context::{
    Context, PendingApproval, PendingQuestion, PermissionState, Principal, SafetyState,
};
pub use dispatch::{DispatchOutcome, Hsm};
pub use effect::{Effect, EffectKind, GatedCall};
pub use error::{MachineError, Result};
pub use event::{Event, EventKind, InternalEvent, ProposedToolCall};
pub use ids::{ApprovalId, CorrelationId, MachineId, QuestionId, TaskId, TimerId, ToolCallId};
pub use machine::Machine;
pub use observation::{CompactionStrategyUsed, ObservationSink, UiEvent};
pub use permissions::{
    capability_for_tool_name, classify, validate_effects_are_allowed, EffectDisposition,
    PermissionPolicy, PermissionPolicyBuilder, ToolCapability,
};
pub use report::{AuditTrailHandle, ErasedReport, RuntimeReport, RuntimeStatus, StateLabel};
pub use snapshot::{RestoreError, Snapshot};
pub use status::Reaction;
pub use submachine::{ErasedMachine, Submachine, SubmachineOutcome};
