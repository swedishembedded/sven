//! The kernel event vocabulary.
//!
//! Events are the **only** thing that can move a machine forward. The LLM, the
//! tool executors, the human, and timers all speak to the kernel exclusively by
//! posting [`Event`]s; nothing reaches into the machine directly. Events record
//! *reality* (what already happened), never intent.
//!
//! # Concrete vs generic
//!
//! Phase 1 deliberately uses a **concrete-but-minimal** `Event` set rather than
//! making the kernel generic over an event type. Payloads that are inherently
//! domain-specific (the text of an LLM proposal, the shape of a tool
//! observation) are kept as [`serde_json::Value`] so the kernel stays fully
//! domain-agnostic while higher crates layer concrete typed payloads on top.
//! This keeps the dispatch engine monomorphic and the public API stable; a
//! fully generic `Event` was evaluated and rejected because it forces every
//! downstream type (`Reaction`, `AuditRecord`, the runtime queue) to carry an
//! extra type parameter for little benefit at this layer.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ApprovalId, TimerId, ToolCallId};

/// Everything the outside world can tell a machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// A human sent a chat/instruction message.
    UserMessage {
        /// Free-form user text.
        text: String,
    },
    /// A human attached an artifact (file, image, blob) referenced opaquely.
    UserProvidedArtifact {
        /// Opaque descriptor of the artifact (domain crates give it meaning).
        artifact: Value,
    },
    /// The human asked to cancel the in-flight work.
    UserCancelled,

    /// The LLM proposed a natural-language response to the user.
    LlmProposedResponse {
        /// Proposed assistant text.
        text: String,
    },
    /// The LLM proposed running a tool. The LLM only *proposes*; the HSM decides.
    LlmProposedToolCall {
        /// Tool name the LLM suggested.
        name: String,
        /// Proposed arguments.
        args: Value,
    },
    /// The LLM proposed a candidate plan / task decomposition.
    LlmProposedPlan {
        /// Structured plan payload.
        plan: Value,
    },
    /// The LLM returned a structured assessment (e.g. completeness, intent).
    LlmProposedAssessment {
        /// Structured assessment payload.
        assessment: Value,
    },
    /// The LLM failed to produce a usable proposal.
    LlmFailed {
        /// Human-readable error.
        error: String,
    },

    /// A state-scoped *deliberation* finished and produced a structured decision.
    ///
    /// Emitted by the deliberation executor after running the model↔tool agentic
    /// loop against a state's append-only conversation thread.  The HSM reads
    /// `decision` (validated against that state's schema) to choose its
    /// transition; the raw final JSON never reaches the observation plane.
    DeliberationComplete {
        /// The conversation thread id this decision belongs to (e.g. `intake`).
        thread: String,
        /// The structured decision payload (already parsed from the model JSON).
        decision: Value,
    },

    /// A tool invocation completed successfully.
    ToolSucceeded {
        /// Matches the [`ToolCallId`] of the originating `CallTool` effect.
        call_id: ToolCallId,
        /// Structured observation produced by the tool.
        observation: Value,
    },
    /// A tool invocation failed.
    ToolFailed {
        /// Matches the [`ToolCallId`] of the originating `CallTool` effect.
        call_id: ToolCallId,
        /// Human-readable error.
        error: String,
    },

    /// A human approved a pending request.
    HumanApproved {
        /// Matches the [`ApprovalId`] of the originating approval request.
        approval_id: ApprovalId,
    },
    /// A human rejected a pending request.
    HumanRejected {
        /// Matches the [`ApprovalId`] of the originating approval request.
        approval_id: ApprovalId,
    },

    /// A scheduled timer elapsed.
    Timeout {
        /// Matches the [`TimerId`] of the originating `ScheduleTimeout` effect.
        timer_id: TimerId,
    },

    /// A kernel-internal event (lifecycle signals + composition signals).
    Internal(InternalEvent),
}

/// Kernel-internal signals.
///
/// [`Entry`](InternalEvent::Entry), [`Exit`](InternalEvent::Exit) and
/// [`Init`](InternalEvent::Init) are the **reserved framework signals** of the
/// HSM algorithm (UML's entry/exit/initial pseudostate). They are dispatched to
/// a *single* state handler (never propagated to a superstate) by the engine.
/// The remaining variants are genuine domain-internal events that flow through
/// the normal hierarchical dispatch like any other event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum InternalEvent {
    /// Reserved: a state is being entered. Handlers may emit effects but must
    /// never request a transition (enforced by a `debug_assert!` in the engine).
    Entry,
    /// Reserved: a state is being exited. Same no-transition rule as `Entry`.
    Exit,
    /// Reserved: fire a composite state's initial transition.
    Init,
    /// A child submachine reached its terminal/Done state.
    SubmachineCompleted {
        /// The machine instance that completed (as a UUID string for serde).
        machine: String,
        /// Result payload the child produced for the parent to consume.
        ///
        /// Carries the child's summary / decision so the parent can enrich its
        /// own thread (append-only).  Defaults to `Null` for children that do
        /// not produce a structured result.
        #[serde(default)]
        result: Value,
    },
    /// A generic, domain-defined internal signal carrying an opaque payload.
    Custom {
        /// Signal name.
        name: String,
        /// Opaque payload.
        payload: Value,
    },
}

impl Event {
    /// The reserved state-entry signal.
    #[must_use]
    pub fn entry() -> Self {
        Event::Internal(InternalEvent::Entry)
    }

    /// The reserved state-exit signal.
    #[must_use]
    pub fn exit() -> Self {
        Event::Internal(InternalEvent::Exit)
    }

    /// The reserved composite-initial-transition signal.
    #[must_use]
    pub fn init() -> Self {
        Event::Internal(InternalEvent::Init)
    }

    /// Convenience constructor for a user message.
    #[must_use]
    pub fn user_message(text: impl Into<String>) -> Self {
        Event::UserMessage { text: text.into() }
    }

    /// Convenience constructor for a timeout event.
    #[must_use]
    pub fn timeout(timer_id: TimerId) -> Self {
        Event::Timeout { timer_id }
    }

    /// `true` for the reserved lifecycle signals (entry/exit/init). The engine
    /// dispatches these directly to one handler and never walks the superstate
    /// chain for them.
    #[must_use]
    pub fn is_lifecycle(&self) -> bool {
        matches!(
            self,
            Event::Internal(InternalEvent::Entry | InternalEvent::Exit | InternalEvent::Init)
        )
    }

    /// The payload-free classification of this event, used for coverage and
    /// continuation logic.
    #[must_use]
    pub fn kind(&self) -> EventKind {
        match self {
            Event::UserMessage { .. } => EventKind::UserMessage,
            Event::UserProvidedArtifact { .. } => EventKind::UserProvidedArtifact,
            Event::UserCancelled => EventKind::UserCancelled,
            Event::LlmProposedResponse { .. } => EventKind::LlmProposedResponse,
            Event::LlmProposedToolCall { .. } => EventKind::LlmProposedToolCall,
            Event::LlmProposedPlan { .. } => EventKind::LlmProposedPlan,
            Event::LlmProposedAssessment { .. } => EventKind::LlmProposedAssessment,
            Event::LlmFailed { .. } => EventKind::LlmFailed,
            Event::DeliberationComplete { .. } => EventKind::DeliberationComplete,
            Event::ToolSucceeded { .. } => EventKind::ToolSucceeded,
            Event::ToolFailed { .. } => EventKind::ToolFailed,
            Event::HumanApproved { .. } => EventKind::HumanApproved,
            Event::HumanRejected { .. } => EventKind::HumanRejected,
            Event::Timeout { .. } => EventKind::Timeout,
            Event::Internal(InternalEvent::Entry) => EventKind::Entry,
            Event::Internal(InternalEvent::Exit) => EventKind::Exit,
            Event::Internal(InternalEvent::Init) => EventKind::Init,
            Event::Internal(InternalEvent::SubmachineCompleted { .. }) => {
                EventKind::SubmachineCompleted
            }
            Event::Internal(InternalEvent::Custom { .. }) => EventKind::Custom,
        }
    }
}

/// Payload-free discriminant of [`Event`].
///
/// Used by continuation logic ("resume after the next answer-shaped event") and
/// by transition-coverage assertions in tests, where comparing the variant
/// without its payload is exactly what is wanted.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum EventKind {
    /// See [`Event::UserMessage`].
    UserMessage,
    /// See [`Event::UserProvidedArtifact`].
    UserProvidedArtifact,
    /// See [`Event::UserCancelled`].
    UserCancelled,
    /// See [`Event::LlmProposedResponse`].
    LlmProposedResponse,
    /// See [`Event::LlmProposedToolCall`].
    LlmProposedToolCall,
    /// See [`Event::LlmProposedPlan`].
    LlmProposedPlan,
    /// See [`Event::LlmProposedAssessment`].
    LlmProposedAssessment,
    /// See [`Event::LlmFailed`].
    LlmFailed,
    /// See [`Event::DeliberationComplete`].
    DeliberationComplete,
    /// See [`Event::ToolSucceeded`].
    ToolSucceeded,
    /// See [`Event::ToolFailed`].
    ToolFailed,
    /// See [`Event::HumanApproved`].
    HumanApproved,
    /// See [`Event::HumanRejected`].
    HumanRejected,
    /// See [`Event::Timeout`].
    Timeout,
    /// Reserved entry signal.
    Entry,
    /// Reserved exit signal.
    Exit,
    /// Reserved init signal.
    Init,
    /// See [`InternalEvent::SubmachineCompleted`].
    SubmachineCompleted,
    /// See [`InternalEvent::Custom`].
    Custom,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_signals_are_flagged() {
        assert!(Event::entry().is_lifecycle());
        assert!(Event::exit().is_lifecycle());
        assert!(Event::init().is_lifecycle());
        assert!(!Event::user_message("hi").is_lifecycle());
        assert!(!Event::UserCancelled.is_lifecycle());
    }

    #[test]
    fn kind_is_payload_free() {
        assert_eq!(Event::user_message("a").kind(), EventKind::UserMessage);
        assert_eq!(Event::user_message("b").kind(), EventKind::UserMessage);
    }
}
