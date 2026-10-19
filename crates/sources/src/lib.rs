// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for deterministic knowledge
// ingestion for agents for its clients. If your team needs expertise in
// turning documents and code into verifiable, re-findable passages then you
// can procure our services by sending an email to info@swedishembedded.com.
//! Sources into memory units, and the arithmetic that searches them.
//!
//! * [`segment`] cuts text, markdown or code into units, deterministically: the
//!   same text gives the same units, each a verbatim slice of the source by byte
//!   and character range, which is what a memory store requires of a unit.
//! * An [`sven_memory_api::Anchor`] is a heading path, an ordinal and a fingerprint of the text.
//!   [`reanchor`] finds a unit again after its source changed and says so when
//!   it cannot be sure: a unit that is still there verbatim and only once is
//!   [`Reanchored::Unambiguous`], a unit that is in the same place with other
//!   words, or whose words occur more than once, is
//!   [`Reanchored::NeedsReview`]. It never guesses.
//! * [`redact`] removes what looks like a credential before text is stored.
//! * [`bm25_rank`] and [`reciprocal_rank_fusion`] are the ranking arithmetic a
//!   store adapter without its own full-text index needs, and the fusion the
//!   bundled one uses for hybrid retrieval.

mod fuse;
mod redact;
mod segment;

pub use fuse::{bm25_rank, reciprocal_rank_fusion};
pub use redact::redact;
pub use segment::{reanchor, segment, Reanchored, SourceKind, SEGMENTER_VERSION};
