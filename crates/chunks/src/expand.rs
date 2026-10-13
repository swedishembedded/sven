// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! What the model reads when it expands a reference: the raw text behind a
//! one-line header that says which chunk and lines it is and whether the
//! source file has changed since the text was read.

use crate::chunk::{ChunkId, ChunkKind};
use crate::freshness::Freshness;
use crate::represent::kind_name;

/// Marks a header whose chunk is out of date. Only the header is ever
/// inspected for it (see [`expansion_is_stale`]), never the content.
const STALE_MARK: &str = "STALE:";

/// A 1-based inclusive line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub first: usize,
    pub last: usize,
}

/// What is being returned.
#[derive(Debug, Clone)]
pub struct ExpansionRequest<'a> {
    pub id: ChunkId,
    pub kind: ChunkKind,
    /// Lines in the whole chunk.
    pub total_lines: usize,
    /// The lines in `text`, or `None` when it is the whole chunk.
    pub range: Option<LineRange>,
    pub freshness: &'a Freshness,
    pub text: &'a str,
}

/// The header line and then `request.text` verbatim.
#[must_use]
pub fn render_expansion(request: &ExpansionRequest<'_>) -> String {
    let extent = match request.range {
        Some(r) => format!("lines {}-{} of {}", r.first, r.last, request.total_lines),
        None => format!("all {} lines", request.total_lines),
    };
    let source = match request.freshness {
        Freshness::Fresh => " | source unchanged since it was read",
        Freshness::Stale { current: None, .. } => {
            " | STALE: the file no longer exists; this is what it contained when it was read"
        }
        Freshness::Stale { .. } => {
            " | STALE: the file changed after it was read; this is what it contained then, read it again for its current content"
        }
        Freshness::Unverifiable => " | source version was not recorded, it may have changed",
        Freshness::NotFileDerived => "",
    };
    format!(
        "[chunk {} | {} | {extent}{source}]\n{}",
        request.id,
        kind_name(request.kind),
        request.text
    )
}

/// Whether the header of an expansion reports a stale source.
#[must_use]
pub fn expansion_is_stale(rendered: &str) -> bool {
    rendered
        .lines()
        .next()
        .is_some_and(|header| header.contains(&format!("| {STALE_MARK}")))
}
