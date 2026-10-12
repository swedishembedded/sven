// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Errors a harness run can produce at its boundaries.

use sven_harness_spec::HarnessError;

/// Everything that can go wrong constructing or resuming a harness run.
///
/// A *running* harness never fails with these: in-run failure is data (the
/// `Failed` state and the run's outcome), not a Rust error, because a
/// machine transition cannot return one.
#[derive(Debug, thiserror::Error)]
pub enum HarnessRunError {
    /// The harness document did not parse, validate or compile.
    #[error("harness document: {0}")]
    Spec(#[from] HarnessError),
    /// The snapshot's state label is not a state this machine can rest in.
    #[error("cannot restore harness snapshot into state `{0}`")]
    Unrestorable(String),
    /// The snapshot's run state was not the JSON shape this machine wrote.
    #[error("harness run state is corrupt: {0}")]
    CorruptRunState(String),
}
