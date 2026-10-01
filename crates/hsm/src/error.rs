//! Kernel error type.

use crate::permissions::ToolCapability;

/// Errors raised by the kernel. The first two variants are the safety-critical
/// ones: they are produced by the permission choke point
/// ([`validate_effects_are_allowed`](crate::permissions::validate_effects_are_allowed))
/// and cause the offending dispatch's effects to be **rejected before any of
/// them executes**.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineError {
    /// A state attempted to use a capability it is not permitted to use.
    #[error("forbidden tool call: capability {capability:?} is not permitted in state `{state}`")]
    ForbiddenToolCall {
        /// Debug label of the state that produced the effect.
        state: String,
        /// The capability that was denied.
        capability: ToolCapability,
    },

    /// The policy asks a person to approve the capability and nobody has.
    #[error(
        "human approval required: capability {capability:?} in state `{state}` needs approval"
    )]
    HumanApprovalRequired {
        /// Debug label of the state that produced the effect.
        state: String,
        /// The capability requiring approval.
        capability: ToolCapability,
    },

    /// The HSM was asked to perform a transition that is not well-formed
    /// (e.g. a target outside the state hierarchy).
    #[error("invalid transition: {0}")]
    InvalidTransition(String),

    /// A submachine composition error.
    #[error("submachine error: {0}")]
    Submachine(String),

    /// A generic runtime failure (channel closed, executor failure, ...).
    #[error("runtime error: {0}")]
    Runtime(String),
}

/// Convenience alias for kernel results.
pub type Result<T> = std::result::Result<T, MachineError>;
