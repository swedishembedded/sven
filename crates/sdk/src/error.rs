// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Failure kinds a call can produce.

/// Why a call to an agent did not produce a result.
///
/// The variants are kept apart on purpose. A model that answered badly, a
/// caller that asked for something impossible, and a transport that fell over
/// are three different problems with three different responses - retrying, and
/// paging someone, are not interchangeable. Collapsing them into one opaque
/// error is what makes an agent service undiagnosable in production.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CallError {
    /// The request was invalid before any generation was attempted.
    ///
    /// Nothing was sent to the model and nothing was spent.
    #[error("{0}")]
    Precondition(String),

    /// The agent's state could not be resumed.
    #[error("cannot resume agent: {0}")]
    Resume(#[from] sven_hsm::RestoreError),

    /// The model never produced a value of the required shape.
    ///
    /// Distinct from [`Self::Postcondition`]: this is a structural failure -
    /// the answer could not be read as the return type at all.
    #[error("the model did not produce a valid {type_name} in {attempts} attempt(s): {detail}")]
    Invalid {
        /// The return type that could not be produced.
        type_name: &'static str,
        /// How many answers were rejected, including the first.
        attempts: u32,
        /// Why the last answer was rejected.
        detail: String,
        /// The last answer, verbatim, for diagnosis.
        last: String,
    },

    /// The model produced a well-formed value that broke an invariant.
    ///
    /// Distinct from [`Self::Invalid`]: the structure was right, so the failure
    /// is about meaning rather than shape. A structurally valid object can
    /// still carry a fabricated citation or an impossible number.
    #[error("{type_name} failed its postcondition in {attempts} attempt(s): {detail}")]
    Postcondition {
        /// The return type whose invariant was broken.
        type_name: &'static str,
        /// How many answers were rejected, including the first.
        attempts: u32,
        /// Which invariant failed.
        detail: String,
        /// The last answer, verbatim, for diagnosis.
        last: String,
    },

    /// A bound given to the call stopped it before a value was produced:
    /// cancelled, out of time, or out of tokens. Not a model mistake.
    #[error("the call was stopped before it produced a value: {conclusion:?}")]
    Stopped {
        /// Which bound stopped it.
        conclusion: crate::RunConclusion,
    },

    /// The kernel, transport or provider failed.
    ///
    /// Not a model mistake. Deliberately distinct so that an outage is never
    /// reported as the model having answered badly.
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}
