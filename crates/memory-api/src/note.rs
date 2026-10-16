// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Notes: sources that are one passage of free text.
//!
//! A person or an agent writing down a fact does not segment it. A note is a
//! source with a single unit, a label taken from its first line, and its
//! entity and origin recorded as exact tags, so every store files a note the
//! same way and a tag filter finds it.
//!
//! Swedish Embedded AB implements solutions for portable agent memory for its
//! clients. If your team needs expertise in keeping an agent's notes in a
//! store that a product can host then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::collections::BTreeSet;

use crate::{IngestRequest, NewUnit, Provenance, SourceId, SpaceId, UnitKind};

/// Prefix of the tag that records what a note is about.
pub const ENTITY_TAG_PREFIX: &str = "entity:";
/// Prefix of the tag that records where a note says it came from.
pub const SOURCE_TAG_PREFIX: &str = "source:";
/// Longest label of a note, in characters.
pub const MAX_LABEL_CHARS: usize = 80;

/// What a note is made of.
#[derive(Clone, Debug)]
pub struct Note<'a> {
    /// The space it goes into.
    pub space: SpaceId,
    /// Its id within the space.
    pub source_id: SourceId,
    /// The text of the note.
    pub text: &'a str,
    /// The person, company or topic it is about.
    pub entity: Option<&'a str>,
    /// Where the writer says it came from. Free text: it labels the note and
    /// is never an input to trust, which `provenance` alone decides.
    pub source: Option<&'a str>,
    /// Tags for filtering.
    pub tags: Vec<String>,
    /// How the note entered memory.
    pub provenance: Provenance,
}

impl Note<'_> {
    /// The request that records this note.
    #[must_use]
    pub fn into_request(self) -> IngestRequest {
        let mut tags: BTreeSet<String> = self.tags.into_iter().collect();
        tags.extend(self.entity.map(|e| format!("{ENTITY_TAG_PREFIX}{e}")));
        tags.extend(self.source.map(|s| format!("{SOURCE_TAG_PREFIX}{s}")));
        IngestRequest {
            space: self.space,
            source_id: self.source_id,
            label: label_of(self.text),
            text: self.text.to_owned(),
            tags,
            provenance: self.provenance,
            units: vec![NewUnit::whole(UnitKind::Prose, self.text)],
        }
    }
}

/// The first line of `text`, cut to [`MAX_LABEL_CHARS`] characters.
#[must_use]
pub fn label_of(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    match first.char_indices().nth(MAX_LABEL_CHARS) {
        Some((end, _)) => format!("{}...", &first[..end]),
        None => first.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Origin, TrainingUse};

    fn note<'a>(text: &'a str, entity: Option<&'a str>) -> Note<'a> {
        Note {
            space: SpaceId::new("personal"),
            source_id: SourceId::new("n-1"),
            text,
            entity,
            source: Some("email"),
            tags: vec!["contact".into()],
            provenance: Provenance {
                origin: Origin::User,
                captured_at: 0,
                training_use: TrainingUse::Unspecified,
            },
        }
    }

    #[test]
    fn a_note_is_one_valid_unit_with_entity_and_source_as_exact_tags() {
        let request = note("Alice prefers calls\nafter lunch", Some("Alice")).into_request();
        assert!(request.validate().is_ok());
        assert_eq!(request.units.len(), 1);
        assert_eq!(request.label, "Alice prefers calls");
        let tags: Vec<_> = request.tags.iter().map(String::as_str).collect();
        assert_eq!(tags, ["contact", "entity:Alice", "source:email"]);
    }

    #[test]
    fn a_long_first_line_is_cut_on_a_character_boundary() {
        let text = "é".repeat(MAX_LABEL_CHARS + 5);
        let label = label_of(&text);
        assert_eq!(label.chars().count(), MAX_LABEL_CHARS + 3);
        assert!(label.ends_with("..."));
    }
}
