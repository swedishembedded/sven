// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The failures every store reports the same way.

use crate::{GenerationId, RevisionId};

/// Why a memory operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryError {
    /// The target does not exist or is outside the caller's spaces; the two
    /// are deliberately indistinguishable.
    #[error("not found")]
    NotFound,
    /// The caller can see the space but was not granted this operation.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The request is malformed, oversized or out of range.
    #[error("invalid request: {0}")]
    InvalidInput(String),
    /// An expected revision no longer matches: someone else got there first.
    /// `current` is the revision to base a retry on.
    #[error("revision conflict, current revision is {current}")]
    Conflict {
        /// The revision the target is at now.
        current: RevisionId,
    },
    /// A query or write named an index generation this store cannot serve.
    #[error("index generation {generation} refused: {reason}")]
    GenerationRefused {
        /// The generation that was named.
        generation: GenerationId,
        /// Why it cannot be used.
        reason: String,
    },
    /// Stored data failed validation on the way out. Never papered over with a
    /// default: a caller must not mistake damaged records for absent ones.
    #[error("stored data is corrupt: {0}")]
    Corrupt(String),
    /// The store cannot be opened or reached.
    #[error("store unavailable: {0}")]
    Unavailable(String),
    /// The storage engine failed.
    #[error("storage failure: {0}")]
    Storage(String),
}
