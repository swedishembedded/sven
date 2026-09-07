// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Where a learned fact came from, and whether that origin is admissible to
//! the durable pending-facts ledger.
//!
//! These are the nouns of the continuous-learning loop. They live in
//! `sven-vocab` (foundation tier) rather than next to the `assimilate_fact`
//! tool because *both* ends of the loop must name the same types and they sit
//! on opposite sides of the architecture:
//!
//! * the resolving tools that produce provenance (`web_fetch`, `ask_question`,
//!   `ingest_document`) live in sibling **domain**-tier crates, which cannot
//!   depend on each other;
//! * the effect executor that observes a real `HumanApproved` event lives in
//!   the **machines** tier, which must not depend on the (SQLite-linking)
//!   memory crate at all - `rusqlite` is forbidden in the `minimal` build.
//!
//! Foundation is the only tier all of them can reach.
//!
//! # The trust model in one paragraph
//!
//! A [`FactSource`] is never asserted by the model. It is attached by whatever
//! *resolved* the information (the fetch that retrieved a page, the question
//! the user answered, the document a human handed over), and the model's own
//! tool-call arguments cannot set or override it. [`FactSource::
//! ledger_admission`] then decides, structurally, what it takes for a fact
//! with that origin to become durable training input.
//!
//! Swedish Embedded AB implements solutions for provenance-tracked knowledge
//! capture in autonomous agents for its clients. If your team needs expertise
//! in trustworthy agent memory then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Hex-encoded content hash of an artifact a human handed over.
///
/// Always computed by the ingesting tool over the bytes it actually read -
/// never taken from a model-supplied argument.
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

/// Identifier of a fact already recorded in the pending-facts ledger.
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
/// The variants are distinguished by the *scope of the human approval* behind
/// them, which is why [`FactSource::UserProvidedDocument`] is not folded into
/// [`FactSource::UserStated`]: the former is approved per content digest and
/// covers many facts the user never read line by line, the latter is approved
/// per utterance. Collapsing them would destroy the ledger's ability to answer
/// "which document, and did a human actually approve *that* document".
///
/// Equally, a handed-over document is categorically not
/// [`FactSource::WebSourced`]: content the agent went and fetched on its own
/// carries no human act at all, and must stay gated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactSource {
    /// The user said it, in their own words, this session.
    UserStated,
    /// Extracted from an artifact the user handed over.
    UserProvidedDocument {
        /// Digest of the ingested artifact; must match an ingested document.
        digest: ContentDigest,
        /// Where the artifact came from (path or URL as given).
        uri: String,
        /// Unix seconds at which the artifact was ingested.
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

/// What it takes for a fact with a given [`FactSource`] to become durable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerAdmission {
    /// Admissible as-is: a human act already covers it.
    Admissible,
    /// Admissible only if this digest matches a document a human ingested.
    RequiresIngestedDocument(ContentDigest),
    /// Admissible only after a real `HumanApproved` event was observed.
    RequiresHumanApproval,
    /// Never admissible, whatever the caller claims.
    Never,
}

impl FactSource {
    /// The structural admissibility rule for this origin.
    ///
    /// `AgentInferred` is deliberately [`LedgerAdmission::Never`]: admitting
    /// inferences by closure over the admissibility of their sources is a real
    /// design with its own failure modes, and is out of scope. Such facts stay
    /// in session memory and never become training input.
    #[must_use]
    pub fn ledger_admission(&self) -> LedgerAdmission {
        match self {
            FactSource::UserStated | FactSource::UserChoice { .. } => LedgerAdmission::Admissible,
            FactSource::UserProvidedDocument { digest, .. } => {
                LedgerAdmission::RequiresIngestedDocument(digest.clone())
            }
            FactSource::WebSourced { .. } => LedgerAdmission::RequiresHumanApproval,
            FactSource::AgentInferred { .. } => LedgerAdmission::Never,
        }
    }

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
}

/// Session-scoped record of human approvals for knowledge assimilation.
///
/// Written **only** by the effect executor that emits a real
/// `Event::HumanApproved` for `ToolCapability::AssimilateKnowledge`, and read
/// by the `assimilate_fact` tool. A boolean the model can set in its own
/// tool-call arguments is a suggestion; this is the gate.
///
/// Shared as an `Arc` between the executor and the tool.
#[derive(Debug, Default)]
pub struct KnowledgeApprovals {
    approved: AtomicBool,
}

impl KnowledgeApprovals {
    /// A fresh, unapproved handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that a human approved knowledge assimilation.
    ///
    /// Call sites are limited to the executor that observed the approval.
    pub fn record_human_approval(&self) {
        self.approved.store(true, Ordering::SeqCst);
    }

    /// `true` once a human approval has been observed this session.
    #[must_use]
    pub fn human_approved(&self) -> bool {
        self.approved.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admissibility_is_decided_by_origin_not_by_assertion() {
        assert_eq!(
            FactSource::UserStated.ledger_admission(),
            LedgerAdmission::Admissible
        );
        assert_eq!(
            FactSource::AgentInferred { from: Vec::new() }.ledger_admission(),
            LedgerAdmission::Never
        );
        assert_eq!(
            FactSource::WebSourced {
                url: "https://example.invalid".into(),
                fetched_at: 0,
                digest: ContentDigest::from_hex("00"),
            }
            .ledger_admission(),
            LedgerAdmission::RequiresHumanApproval
        );
        assert_eq!(
            FactSource::UserProvidedDocument {
                digest: ContentDigest::from_hex("ab"),
                uri: "file:///x".into(),
                ingested_at: 0,
                span: 0..1,
            }
            .ledger_admission(),
            LedgerAdmission::RequiresIngestedDocument(ContentDigest::from_hex("ab"))
        );
    }

    #[test]
    fn a_fresh_approvals_handle_has_approved_nothing() {
        let approvals = KnowledgeApprovals::new();
        assert!(!approvals.human_approved());
        approvals.record_human_approval();
        assert!(approvals.human_approved());
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
