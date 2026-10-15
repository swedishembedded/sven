// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Builders shared by the conformance cases.

use std::collections::BTreeSet;

use crate::{
    AccessContext, Anchor, IngestRequest, NewUnit, Origin, Permission, Provenance, SourceId,
    SpaceId, Span, TrainingUse, UnitKind,
};

pub(super) fn space(id: &str) -> SpaceId {
    SpaceId::new(id)
}

/// A caller that reads and fully writes each of `spaces`.
pub(super) fn owner_of(spaces: &[&str]) -> AccessContext {
    spaces.iter().fold(AccessContext::new("tester"), |ctx, id| {
        ctx.writing(
            space(id),
            &[Permission::Ingest, Permission::Annotate, Permission::Delete],
        )
    })
}

pub(super) fn provenance(origin: Origin) -> Provenance {
    Provenance {
        origin,
        captured_at: 1_700_000_000,
        training_use: TrainingUse::Unspecified,
    }
}

/// A source whose units are its blank-line separated paragraphs, with exact
/// byte and character ranges.
pub(super) fn source(space_id: &str, source_id: &str, text: &str) -> IngestRequest {
    source_with(space_id, source_id, text, &[], Origin::User)
}

pub(super) fn source_with(
    space_id: &str,
    source_id: &str,
    text: &str,
    tags: &[&str],
    origin: Origin,
) -> IngestRequest {
    let mut units = Vec::new();
    let mut byte = 0;
    let mut chars = 0;
    for (ordinal, paragraph) in text.split("\n\n").enumerate() {
        units.push(NewUnit {
            kind: UnitKind::Prose,
            anchor: Anchor {
                heading_path: Vec::new(),
                block_ordinal: ordinal as u32,
                text_fingerprint: crate::sha256_hex(paragraph.as_bytes()),
            },
            byte_range: Span {
                start: byte,
                end: byte + paragraph.len(),
            },
            char_range: Span {
                start: chars,
                end: chars + paragraph.chars().count(),
            },
            text: paragraph.to_owned(),
        });
        byte += paragraph.len() + 2;
        chars += paragraph.chars().count() + 2;
    }
    IngestRequest {
        space: space(space_id),
        source_id: SourceId::new(source_id),
        label: source_id.to_owned(),
        text: text.to_owned(),
        tags: tags
            .iter()
            .map(|t| (*t).to_owned())
            .collect::<BTreeSet<_>>(),
        provenance: provenance(origin),
        units,
    }
}
