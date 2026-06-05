//! Error type for LLM adapter operations.

use thiserror::Error;

/// Errors that can occur when invoking the LLM reasoning service.
#[derive(Debug, Error)]
pub enum LlmError {
    /// The LLM returned text that could not be parsed into the expected response shape.
    #[error("parse failure for {request_kind}: {detail}")]
    ParseFailure {
        /// The request variant that produced this error.
        request_kind: &'static str,
        /// What went wrong.
        detail: String,
    },

    /// The underlying model provider returned an error.
    #[error("provider error: {0}")]
    ProviderError(String),

    /// The adapter produced an event of the wrong kind (internal logic error).
    #[error("unexpected event kind returned by adapter")]
    UnexpectedEventKind,
}

impl From<anyhow::Error> for LlmError {
    fn from(e: anyhow::Error) -> Self {
        LlmError::ProviderError(e.to_string())
    }
}
