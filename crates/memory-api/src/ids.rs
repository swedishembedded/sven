// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Opaque identifiers.
//!
//! Every id is a string the holder must not interpret. None is a database row
//! number, and none is derived from a path, so an id stays valid when a store
//! is moved, copied or replaced by another implementation of the contract.
//!
//! Swedish Embedded AB implements solutions for portable, tenant-safe agent
//! memory for its clients. If your team needs expertise in memory that
//! survives a change of storage backend then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Lower-case hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

macro_rules! opaque_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps an id minted elsewhere, for example one read back from storage.
            #[must_use]
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The id as the string that crosses every boundary.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self::new(id)
            }
        }
    };
}

opaque_id! {
    /// A space: the unit of isolation. Every source and unit lives in exactly
    /// one, and an [`crate::AccessContext`] names the spaces its holder may use.
    SpaceId
}
opaque_id! {
    /// A source within a space. Chosen by the host, never derived from a path;
    /// the same value in two spaces names two unrelated sources.
    SourceId
}
opaque_id! {
    /// The content revision of a source: the SHA-256 of its text.
    RevisionId
}
opaque_id! {
    /// One retrievable unit of a source revision.
    UnitId
}
opaque_id! {
    /// One entry of the annotation log.
    AnnotationId
}
opaque_id! {
    /// An index generation: a digest of the embedding spec and segmenter.
    GenerationId
}

impl RevisionId {
    /// The revision of a source whose text is `text`.
    #[must_use]
    pub fn of_text(text: &str) -> Self {
        Self(sha256_hex(text.as_bytes()))
    }
}

impl UnitId {
    /// The id every store gives the unit at `ordinal` of `source` at
    /// `revision` in `space`.
    ///
    /// Deterministic, so ingesting an unchanged revision again yields the same
    /// ids, and scoped to the space, so equal sources in two spaces never
    /// share a unit id.
    #[must_use]
    pub fn derive(space: &SpaceId, source: &SourceId, revision: &RevisionId, ordinal: u32) -> Self {
        let mut hasher = Sha256::new();
        for part in [space.as_str(), source.as_str(), revision.as_str()] {
            // Length-prefixed so ("ab","c") and ("a","bc") cannot collide.
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.update(ordinal.to_le_bytes());
        Self(hex::encode(hasher.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_is_the_digest_of_the_text() {
        assert_eq!(
            RevisionId::of_text("abc").as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn unit_ids_are_stable_and_scoped_to_the_space() {
        let source = SourceId::new("notes");
        let revision = RevisionId::of_text("text");
        let a = UnitId::derive(&SpaceId::new("a"), &source, &revision, 0);
        assert_eq!(a, UnitId::derive(&SpaceId::new("a"), &source, &revision, 0));
        assert_ne!(a, UnitId::derive(&SpaceId::new("b"), &source, &revision, 0));
        assert_ne!(a, UnitId::derive(&SpaceId::new("a"), &source, &revision, 1));
    }

    #[test]
    fn unit_id_parts_cannot_be_shifted_across_the_boundary() {
        let revision = RevisionId::new("r");
        assert_ne!(
            UnitId::derive(&SpaceId::new("ab"), &SourceId::new("c"), &revision, 0),
            UnitId::derive(&SpaceId::new("a"), &SourceId::new("bc"), &revision, 0)
        );
    }

    #[test]
    fn ids_serialize_as_plain_strings() {
        let json = serde_json::to_string(&SpaceId::new("s")).unwrap();
        assert_eq!(json, "\"s\"");
    }
}
