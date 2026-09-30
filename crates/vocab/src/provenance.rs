// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Where a piece of information a tool resolved came from.
//!
//! A tool that *resolves* information - rather than acting on the model's own
//! arguments - attaches a [`FactSource`] to its result
//! (`sven_vocab::ToolOutput::provenance`). The vocabulary lives in
//! `sven-vocab` (foundation tier) because the tools that attach it
//! (`web_fetch`, `web_search`, `ask_question`) sit in sibling domain-tier
//! crates that cannot depend on each other, and everything downstream that
//! reads it - semantic memory's recall framing, an embedding application -
//! has to name the same types.
//!
//! # The trust model in one paragraph
//!
//! A [`FactSource`] is never asserted by the model. It is attached by whatever
//! *resolved* the information (the fetch that retrieved a page, the question
//! the user answered, the document a human handed over), and the model's own
//! tool-call arguments cannot set or override it.
//! [`FactSource::recalls_as_untrusted`] then decides, structurally, whether
//! content with that origin may be read back to the model as ground truth.
//!
//! Swedish Embedded AB implements solutions for provenance-tracked knowledge
//! capture in autonomous agents for its clients. If your team needs expertise
//! in trustworthy agent memory then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Hex-encoded content hash of an artifact.
///
/// Always computed by the tool that read the artifact, over the bytes it
/// actually read - never taken from a model-supplied argument.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentDigest(String);

impl ContentDigest {
    /// Wraps an already hex-encoded digest.
    #[must_use]
    pub fn from_hex(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    /// The hex-encoded digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of a fact another fact was derived from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactId(String);

impl FactId {
    /// Wraps an existing identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a fact came to be known.
///
/// The variants are distinguished by the *scope of the human act* behind
/// them, which is why [`FactSource::UserProvidedDocument`] is not folded into
/// [`FactSource::UserStated`]: the former covers a whole artifact the user
/// handed over, identified by its digest, and many facts the user never read
/// line by line; the latter covers one utterance.
///
/// Equally, a handed-over document is categorically not
/// [`FactSource::WebSourced`]: content the agent went and fetched on its own
/// carries no human act at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactSource {
    /// The user said it, in their own words, this session.
    UserStated,
    /// Extracted from an artifact the user handed over.
    UserProvidedDocument {
        /// Digest of the artifact as it was read.
        digest: ContentDigest,
        /// Where the artifact came from (path or URL as given).
        uri: String,
        /// Unix seconds at which the artifact was read.
        ingested_at: u64,
        /// Byte range within the artifact the fact was extracted from.
        span: Range<usize>,
    },
    /// Retrieved by the agent from the network on its own initiative.
    WebSourced {
        /// The URL actually fetched.
        url: String,
        /// Unix seconds at which it was fetched.
        fetched_at: u64,
        /// Digest of the fetched content.
        digest: ContentDigest,
    },
    /// Derived by the model from other facts.
    AgentInferred {
        /// The facts it was derived from.
        from: Vec<FactId>,
    },
    /// The user picked one option over the others that were offered.
    UserChoice {
        /// Identifies the question that was asked.
        question_id: String,
        /// The option the user picked.
        chosen: String,
        /// The options the user did not pick.
        not_chosen: Vec<String>,
    },
}

impl FactSource {
    /// Short stable label for metadata and human-readable output.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            FactSource::UserStated => "user_stated",
            FactSource::UserProvidedDocument { .. } => "user_provided_document",
            FactSource::WebSourced { .. } => "web_sourced",
            FactSource::AgentInferred { .. } => "agent_inferred",
            FactSource::UserChoice { .. } => "user_choice",
        }
    }

    /// Whether content with this origin must be recalled into the model's
    /// prompt as quoted, explicitly-untrusted material rather than as a plain
    /// assertion.
    ///
    /// `AgentInferred` is the agent's own reasoning over untrusted material,
    /// and a `WebSourced` fact - approved or not - is a quotation of a page,
    /// not testimony.
    #[must_use]
    pub fn recalls_as_untrusted(&self) -> bool {
        match self {
            FactSource::WebSourced { .. } | FactSource::AgentInferred { .. } => true,
            FactSource::UserStated
            | FactSource::UserProvidedDocument { .. }
            | FactSource::UserChoice { .. } => false,
        }
    }
}

/// [`FactSource::recalls_as_untrusted`], decided from the stored
/// [`FactSource::label`] alone.
///
/// The recall path only ever has the label: provenance crosses into the memory
/// store as a metadata string. Keeping both spellings of the rule in this one
/// module - and pinning that they agree, for every variant, in this module's
/// own tests - is what stops them drifting apart.
#[must_use]
pub fn label_recalls_as_untrusted(label: &str) -> bool {
    matches!(label, "web_sourced" | "agent_inferred")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_spelling_of_the_recall_rule_agrees_with_the_typed_one() {
        for source in [
            FactSource::UserStated,
            FactSource::UserProvidedDocument {
                digest: ContentDigest::from_hex("ab"),
                uri: "file:///x".into(),
                ingested_at: 0,
                span: 0..1,
            },
            FactSource::WebSourced {
                url: "https://example.invalid".into(),
                fetched_at: 0,
                digest: ContentDigest::from_hex("00"),
            },
            FactSource::AgentInferred { from: Vec::new() },
            FactSource::UserChoice {
                question_id: "q1".into(),
                chosen: "a".into(),
                not_chosen: vec!["b".into()],
            },
        ] {
            assert_eq!(
                label_recalls_as_untrusted(source.label()),
                source.recalls_as_untrusted(),
                "the two spellings disagree for {source:?}"
            );
        }
    }

    #[test]
    fn fact_source_round_trips_through_json() {
        let source = FactSource::UserProvidedDocument {
            digest: ContentDigest::from_hex("abc123"),
            uri: "file:///spec.md".into(),
            ingested_at: 42,
            span: 3..9,
        };
        let json = serde_json::to_string(&source).expect("serialize");
        let back: FactSource = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(source, back);
    }
}
