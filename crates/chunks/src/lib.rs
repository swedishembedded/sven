// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Addressable context.
//!
//! Raw evidence (a file read, a command's output, a test run) is stored once,
//! outside the transcript, under an id derived from its content. The
//! transcript carries a *reference* and a chosen *representation*; the raw
//! text can always be fetched again by id.
//!
//! - [`ContextChunk`] is the record: kind, provenance, size, flags.
//! - [`ChunkStore`] keeps payloads on disk, a bounded set of them resident,
//!   and reports whether a file-derived chunk is still current.
//! - Representations of a chunk at each [`Visibility`] are produced by
//!   deterministic extractors, never by a model.
//!
//! Chunk text is untrusted evidence. Nothing here interprets it.

mod chunk;
mod expand;
mod freshness;
mod represent;
mod store;

pub use chunk::{
    content_digest, ChunkDraft, ChunkId, ChunkIdError, ChunkKind, ContentRef, ContextChunk,
    Lifetime, Provenance, Sensitivity, BYTES_PER_TOKEN,
};
pub use expand::{expansion_is_stale, render_expansion, ExpansionRequest, LineRange};
pub use freshness::{Freshness, FsProbe, ProbeError, SourceProbe, SourceVersion};
pub use represent::{
    allowed_visibilities, indicates_failure, render, tokens_of, RenderRequest, Rendered,
    Visibility, EXPAND_ACTION, EXPAND_TOOL_NAME,
};
pub use store::{ChunkStore, PutOutcome, StoreLimits};
