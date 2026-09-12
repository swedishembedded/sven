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
use sven_vocab::verify::VerifierVerdict;

use crate::ids::{ApprovalId, QuestionId, TimerId, ToolCallId};
use crate::permissions::ToolCapability;

/// A single tool call proposed by the LLM in a `LlmTurnComplete` event.
///
/// The executor (which has registry access) annotates `capability` so machines
/// stay pure and domain-only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposedToolCall {
    /// Call identifier assigned by the model (forwarded to `Effect::CallTool`).
    pub call_id: ToolCallId,
    /// Tool name the LLM suggested.
    pub name: String,
    /// Arguments as a JSON object.
    pub args: Value,
    /// Kernel capability bucket (annotated by `TurnExecutor`, not by the machine).
    pub capability: ToolCapability,
}

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

    /// A single streaming LLM turn completed: the model produced text and/or
    /// proposed tool calls.
    ///
    /// Emitted by `TurnExecutor` after the stream ends.  The machine reads
    /// `tool_calls` to decide whether to emit `Effect::CallTool` effects
    /// (entering `RunningTools`) or to finalize with the `text` response (going
    /// to `Idle`).  Live text deltas have already been streamed via `UiEvent`s
    /// on the outward plane; `text` is the complete concatenation for the
    /// machine's logic.
    LlmTurnComplete {
        /// The conversation thread this turn belongs to (e.g. `"chat"`,
        /// `"discovery"`).
        thread: String,
        /// Complete assistant text, or empty string if the turn was tool-only.
        text: String,
        /// All tool calls proposed by the LLM, capability-annotated.
        tool_calls: Vec<ProposedToolCall>,
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

    /// The permission gate requires approval before a tool call can proceed.
    ///
    /// Emitted by `run_effects` when a `CallTool` effect classifies as
    /// `NeedsApproval`.  The machine should transition to `AwaitingApproval`,
    /// emit `Effect::RequestHumanApproval`, and on `HumanApproved` re-emit the
    /// `CallTool` (which will then classify as `Allowed` because `ctx.grant`
    /// has been called).  On `HumanRejected` the machine synthesizes a
    /// `ToolFailed` result back into the agentic loop.
    ToolApprovalRequired {
        /// Matches the [`ToolCallId`] of the pending `CallTool` effect.
        call_id: ToolCallId,
        /// The capability bucket that needs approval.
        capability: ToolCapability,
        /// Human-readable description of the operation requesting approval.
        description: String,
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

    /// A tool call cannot be resolved without asking a human, and the answer
    /// may not arrive for a while - the run must park rather than block.
    ///
    /// Emitted by an executor in place of `ToolSucceeded`/`ToolFailed` when a
    /// tool (e.g. `ask_question`) has no conclusive answer of its own. The
    /// machine removes `call_id` from its pending set (it will not produce a
    /// `ToolSucceeded`), records a [`crate::context::PendingQuestion`], and
    /// emits `Effect::RequestHumanAnswer`. Unlike tool approval, resolving
    /// this does **not** re-run anything: the answer *is* the tool result, so
    /// the executor that later posts `HumanAnswered` is responsible for
    /// appending it as the tool-result message before posting the event -
    /// the machine only resumes the loop.
    QuestionAsked {
        /// Matches the [`ToolCallId`] of the pending `CallTool` effect.
        call_id: ToolCallId,
        /// The question text shown to the human.
        prompt: String,
        /// Offered choices, if any (empty for a free-form question).
        options: Vec<String>,
    },
    /// A human answered a previously parked question.
    HumanAnswered {
        /// Matches the [`QuestionId`] of the originating `QuestionAsked`.
        question_id: QuestionId,
        /// The human's answer, already appended to the thread as this call's
        /// tool-result message by whoever posts this event.
        answer: String,
    },

    /// A scheduled timer elapsed.
    Timeout {
        /// Matches the [`TimerId`] of the originating `ScheduleTimeout` effect.
        timer_id: TimerId,
    },

    /// An `Effect::Verify` finished evaluating.
    ///
    /// The *only* way a verdict reaches a machine - see `Effect::Verify`'s
    /// doc. Carries no correlating id: today at most one verification is ever
    /// in flight per machine instance (the verified-task machine's
    /// `Verifying` state), so there is nothing to correlate against yet. A
    /// second concurrent consumer would need one added, not assumed.
    VerificationComplete {
        /// What the verifier concluded.
        verdict: VerifierVerdict,
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
            Event::LlmTurnComplete { .. } => EventKind::LlmTurnComplete,
            Event::DeliberationComplete { .. } => EventKind::DeliberationComplete,
            Event::ToolSucceeded { .. } => EventKind::ToolSucceeded,
            Event::ToolFailed { .. } => EventKind::ToolFailed,
            Event::ToolApprovalRequired { .. } => EventKind::ToolApprovalRequired,
            Event::HumanApproved { .. } => EventKind::HumanApproved,
            Event::HumanRejected { .. } => EventKind::HumanRejected,
            Event::QuestionAsked { .. } => EventKind::QuestionAsked,
            Event::HumanAnswered { .. } => EventKind::HumanAnswered,
            Event::Timeout { .. } => EventKind::Timeout,
            Event::VerificationComplete { .. } => EventKind::VerificationComplete,
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
    /// See [`Event::LlmTurnComplete`].
    LlmTurnComplete,
    /// See [`Event::DeliberationComplete`].
    DeliberationComplete,
    /// See [`Event::ToolSucceeded`].
    ToolSucceeded,
    /// See [`Event::ToolFailed`].
    ToolFailed,
    /// See [`Event::ToolApprovalRequired`].
    ToolApprovalRequired,
    /// See [`Event::HumanApproved`].
    HumanApproved,
    /// See [`Event::HumanRejected`].
    HumanRejected,
    /// See [`Event::QuestionAsked`].
    QuestionAsked,
    /// See [`Event::HumanAnswered`].
    HumanAnswered,
    /// See [`Event::Timeout`].
    Timeout,
    /// See [`Event::VerificationComplete`].
    VerificationComplete,
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
