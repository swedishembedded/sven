// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Index generations.
//!
//! Vectors from two embedding models are not comparable, so an index is only
//! ever read by a query that names the generation it was built for. A change
//! of model, tokenizer, pooling or prefixing is a new generation; the old one
//! is kept until the new one is active and is then retired, never edited.

use serde::{Deserialize, Serialize};

use crate::{ids::sha256_hex, GenerationId};

/// How text is prepared before it is embedded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum PromptStyle {
    /// The text is embedded as it is.
    Plain,
    /// Queries and documents each get a fixed prefix.
    Prefixes {
        /// Prepended to a query.
        query: String,
        /// Prepended to a document.
        document: String,
    },
    /// Queries are wrapped in an instruction; documents are embedded as is.
    Instruction {
        /// The instruction text.
        text: String,
    },
}

/// Everything that determines what an embedding vector means.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingSpec {
    /// The model's name.
    pub model_id: String,
    /// Digest of the model weights, so a silently replaced file is a new spec.
    pub weights_digest: String,
    /// Digest of the tokenizer files.
    pub tokenizer_digest: String,
    /// How token states are pooled into one vector.
    pub pooling: String,
    /// Whether vectors are scaled to unit length.
    pub normalized: bool,
    /// Vector length.
    pub dims: usize,
    /// How text is prepared.
    pub prompt: PromptStyle,
    /// Longest input in tokens.
    pub max_tokens: usize,
}

impl EmbeddingSpec {
    /// The bytes the generation id is computed from. Field order is the
    /// declaration order, so the form is stable.
    fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an embedding spec is plain data and always serializes")
    }

    /// The generation this spec yields together with `segmenter_version`.
    #[must_use]
    pub fn generation_id(&self, segmenter_version: &str) -> GenerationId {
        let mut bytes = self.canonical();
        bytes.push(0);
        bytes.extend_from_slice(segmenter_version.as_bytes());
        GenerationId::new(sha256_hex(&bytes))
    }
}

/// Where a generation is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GenerationState {
    /// Being filled; not yet served to queries.
    Building,
    /// Served to queries.
    Active,
    /// Superseded; kept for rollback and refused by queries.
    Retired,
}

/// A registered index generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGeneration {
    /// `sha256(canonical(spec) + segmenter_version)`.
    pub generation_id: GenerationId,
    /// The embedding spec its vectors follow.
    pub spec: EmbeddingSpec,
    /// The version of the segmenter that cut its units.
    pub segmenter_version: String,
    /// Its state.
    pub state: GenerationState,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> EmbeddingSpec {
        EmbeddingSpec {
            model_id: "m".into(),
            weights_digest: "w".into(),
            tokenizer_digest: "t".into(),
            pooling: "mean".into(),
            normalized: true,
            dims: 4,
            prompt: PromptStyle::Plain,
            max_tokens: 512,
        }
    }

    #[test]
    fn any_change_of_spec_or_segmenter_starts_a_new_generation() {
        let base = spec().generation_id("seg1");
        assert_eq!(base, spec().generation_id("seg1"));
        assert_ne!(base, spec().generation_id("seg2"));
        let mut other = spec();
        other.weights_digest = "w2".into();
        assert_ne!(base, other.generation_id("seg1"));
        let mut other = spec();
        other.prompt = PromptStyle::Instruction {
            text: "find".into(),
        };
        assert_ne!(base, other.generation_id("seg1"));
    }
}
