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
use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Whether content with this origin must be recalled into the model's
    /// prompt as quoted, explicitly-untrusted material rather than as a plain
    /// assertion.
    ///
    /// Admissibility ([`Self::ledger_admission`]) and recall framing are
    /// deliberately separate questions: the ledger asks "may this become
    /// training input", this asks "may the model read it as ground truth".
    /// `AgentInferred` never reaches the ledger yet is the agent's own
    /// reasoning over untrusted material, and an *approved* `WebSourced` fact
    /// is admissible yet is still a quotation of a page, not testimony.
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

/// Where a resolving tool's attached provenance is recorded, keyed by a
/// handle the model can later cite as `assimilate_fact`'s `evidence`
/// argument - in practice, that tool call's own id, which the model already
/// knows because it minted it.
///
/// Implemented by `sven-memory`'s `ProvenanceIndex`. Named here, in
/// foundation tier, rather than used directly by name, because the impure
/// I/O layer that runs a resolving tool (`sven-executors`, machines tier)
/// must record into it without depending on the SQLite-linking `sven-memory`
/// crate, which is kept out of the `minimal` build's dependency closure.
pub trait ProvenanceSink: Send + Sync {
    /// Records the provenance a resolving tool call established for `handle`.
    fn record_provenance(&self, handle: &str, source: FactSource);
}

/// Session-scoped record of human approvals for knowledge assimilation.
///
/// Written **only** by the effect executor that emits a real
/// `Event::HumanApproved` for `ToolCapability::AssimilateKnowledge`, and read
/// by the `assimilate_fact` tool. A boolean the model can set in its own
/// tool-call arguments is a suggestion; this is the gate.
///
/// # Each approval is single-use
///
/// An approval is *consumed* by the admission it pays for
/// ([`Self::consume_approval`]), so one human act admits exactly one
/// agent-initiated fact. A latch would be a far weaker gate than it looks: the
/// human is shown one specific thing, and everything the agent fetched
/// afterwards - from URLs and digests no human ever saw - would ride in on that
/// single click for the rest of the session.
///
/// This is deliberately *not* the same shape as
/// [`crate::provenance::FactSource::UserProvidedDocument`]'s rule. That one is
/// admissible without a per-fact approval precisely because it is scoped to a
/// digest a human handed over; a web fetch has no such scope, so it pays per
/// fact. Binding an approval to the *specific* source it was granted for would
/// be stronger still, but the approval request carries only a capability and a
/// description today - the milestone that mints an `AssimilateKnowledge`
/// approval request is the one that can add that scope.
///
/// Shared as an `Arc` between the executor and the tool.
#[derive(Debug, Default)]
pub struct KnowledgeApprovals {
    /// Approvals observed but not yet spent on an admission.
    unspent: AtomicUsize,
}

impl KnowledgeApprovals {
    /// A fresh handle with no approvals.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that a human approved one knowledge assimilation.
    ///
    /// Call sites are limited to the executor that observed the approval.
    pub fn record_human_approval(&self) {
        self.unspent.fetch_add(1, Ordering::SeqCst);
    }

    /// Spends one unspent approval, returning `true` if there was one.
    ///
    /// The caller may admit exactly one fact per `true`. Racing callers each
    /// get their own approval or none: the decrement is atomic.
    #[must_use]
    pub fn consume_approval(&self) -> bool {
        self.unspent
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }

    /// `true` while an approval is available to spend.
    ///
    /// A query only - it does not spend anything. Use
    /// [`Self::consume_approval`] to gate an admission.
    #[must_use]
    pub fn human_approved(&self) -> bool {
        self.unspent.load(Ordering::SeqCst) > 0
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
    fn a_fresh_approvals_handle_has_approved_nothing() {
        let approvals = KnowledgeApprovals::new();
        assert!(!approvals.human_approved());
        approvals.record_human_approval();
        assert!(approvals.human_approved());
    }

    #[test]
    fn an_approval_is_spent_by_the_admission_it_pays_for() {
        let approvals = KnowledgeApprovals::new();
        assert!(!approvals.consume_approval(), "nothing to spend");

        approvals.record_human_approval();
        assert!(approvals.consume_approval(), "the one approval is spendable");
        assert!(
            !approvals.consume_approval(),
            "one human act must not pay for a second admission"
        );
        assert!(!approvals.human_approved());

        // Approvals accumulate: two human acts pay for two admissions.
        approvals.record_human_approval();
        approvals.record_human_approval();
        assert!(approvals.consume_approval());
        assert!(approvals.consume_approval());
        assert!(!approvals.consume_approval());
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
