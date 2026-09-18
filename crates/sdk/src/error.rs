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
pub enum CallError {
    /// The request was invalid before any generation was attempted.
    ///
    /// Nothing was sent to the model and nothing was spent.
    #[error("{0}")]
    Precondition(String),

    /// The agent's state could not be resumed.
    #[error("cannot resume agent: {0}")]
    Resume(#[from] sven_hsm::RestoreError),

    /// The kernel, transport or provider failed.
    ///
    /// Not a model mistake. Deliberately distinct so that an outage is never
    /// reported as the model having answered badly.
    #[error(transparent)]
    Infrastructure(#[from] anyhow::Error),
}
