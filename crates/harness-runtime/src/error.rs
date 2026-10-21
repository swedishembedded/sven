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
    /// A harness name was registered twice; a registry is a set, not a
    /// scratchpad.
    #[error("harness `{0}` is already registered")]
    DuplicateHarness(String),
    /// A checkpoint was taken under a different version of the plan than
    /// the document offered at resume. The checkpoint carries the digest of
    /// the plan it started under; resuming under an edited document would
    /// replay recorded results through changed steps, so it refuses.
    #[error(
        "harness `{name}`: the checkpoint was taken under digest {checkpoint}, but the \
         document compiles to {document}; a modified harness cannot replace a checkpointed \
         plan"
    )]
    PinnedPlanMismatch {
        /// The harness name the checkpoint and the document share.
        name: String,
        /// The digest recorded in the checkpoint's run state.
        checkpoint: String,
        /// The digest the offered document compiles to.
        document: String,
    },
    /// A checkpoint was taken under a bundle, and the offered resume is not
    /// that bundle (or offers no bundle at all). A bundle's helpers are
    /// part of the program's meaning; replaying recorded steps through
    /// different helper bytes would not be the same run.
    #[error(
        "harness `{name}`: the checkpoint was taken under bundle digest {checkpoint}, \
         but {offered} was offered; resume requires the same bundle"
    )]
    PinnedBundleMismatch {
        /// The harness name the checkpoint and the bundle share.
        name: String,
        /// The bundle digest recorded in the checkpoint's run state.
        checkpoint: String,
        /// What the resume offered instead of that bundle.
        offered: String,
    },
}
