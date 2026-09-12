// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `assimilate_fact` - the one writer into durable agent knowledge.
//!
//! Every fact the agent learns goes through this tool and no other. One writer
//! means one gate: resolving tools (`web_fetch`, `ask_question`,
//! `ingest_document`) attach provenance, they never write memory or the ledger
//! themselves, because two writers is how a gate gets bypassed by accident six
//! months later.
//!
//! # What the model can and cannot decide
//!
//! The model chooses *what* to remember and which piece of resolved evidence it
//! came from. It cannot choose *where the fact came from* and it cannot declare
//! it confirmed:
//!
//! * the [`FactSource`] is looked up in the [`ProvenanceIndex`] under the
//!   evidence handle the resolution path minted - a `source` field in the
//!   tool-call arguments is not read at all, and an unknown or absent handle
//!   resolves to [`FactSource::AgentInferred`], which never reaches the ledger;
//! * human confirmation is taken from [`KnowledgeApprovals`], which only the
//!   effect executor that observed a real `HumanApproved` event writes - a
//!   `confirmed` field in the tool-call arguments is not read at all. Each
//!   approval is *spent* by the one fact it admits, so a single human act
//!   never ungates the rest of the session.
//!
//! The fact is *always* written to semantic memory (instant recall this
//! session). Only the durable ledger - what later feeds training - is gated.
//!
//! Semantic memory being ungated does not make it a free channel into the
//! prompt: the record carries its resolved provenance, and [`crate::recall`]
//! decides on the way back out how it may be framed and which sessions may see
//! it at all.
//!
//! Swedish Embedded AB implements solutions for gated knowledge assimilation in
//! autonomous agents for its clients. If your team needs expertise in keeping
//! untrusted content out of a model's weights then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
    ToolCapability,
};
use sven_vocab::provenance::{FactId, FactSource, KnowledgeApprovals, LedgerAdmission, ProvenanceSink};

use crate::{
    ledger::{FrozenProbe, PendingFactRecord, PendingFactsLedger},
    recall::{SessionScope, PROVENANCE_KEY, SESSION_SCOPE_KEY},
    store::{Document, VectorStore},
};

/// The probe this call froze for `fact`, if it froze one.
///
/// Three outcomes, and the middle one is the point: no probe at all is fine
/// (the fact is knowledge that simply cannot be scored, and the submitter says
/// so), a *complete* probe is taken verbatim, and anything in between is an
/// error rather than a silently dropped half. Half a probe is almost always a
/// model that meant to write one, and dropping it would send an unscoreable
/// fact to training under the impression it was scoreable.
///
/// The containment rule mirrors brain's own `FactBatch` validation, which
/// refuses such a batch outright. Catching it here turns an opaque failure
/// several subprocesses later into a tool error the model can act on
/// immediately.
fn frozen_probe(args: &Value, fact: &str) -> Result<Option<FrozenProbe>, String> {
    let field = |name: &str| {
        args.get(name)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    match (field("probe_question"), field("expected_answer")) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(
            "assimilate_fact: 'probe_question' needs an 'expected_answer' - a question \
             nothing can be scored against is not a probe"
                .to_string(),
        ),
        (None, Some(_)) => Err(
            "assimilate_fact: 'expected_answer' needs a 'probe_question' - an answer to \
             no question cannot be asked of the model"
                .to_string(),
        ),
        (Some(question), Some(expected_answer)) => {
            if normalized(fact).contains(&normalized(question)) {
                return Err(format!(
                    "assimilate_fact: the probe question {question:?} appears inside the fact \
                     it is meant to test, so answering it needs no knowledge of the fact. \
                     Ask something the fact ANSWERS, in different words."
                ));
            }
            Ok(Some(FrozenProbe {
                question: question.to_string(),
                expected_answer: expected_answer.to_string(),
            }))
        }
    }
}

/// Case- and whitespace-insensitive text, matching the normalisation brain
/// applies before it makes the same containment check. Two spellings of one
/// leak are one leak.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .to_lowercase()
}

/// Session-scoped map from an evidence handle to the provenance the resolution
/// path recorded for it.
///
/// Populated by whatever actually resolved the information; read by
/// [`AssimilateFactTool`]. Nothing the model writes reaches this map.
#[derive(Debug, Default)]
pub struct ProvenanceIndex {
    entries: Mutex<HashMap<String, FactSource>>,
}

impl ProvenanceIndex {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the provenance a resolution step established for `handle`.
    ///
    /// A `WebSourced` record naming no URL is refused outright - never
    /// inserted, not even as a claim to be caught by the admissibility check
    /// later - because there is nothing here a human could have been shown.
    /// This is a fabricated or missing provenance claim, not a default to
    /// fall back on: refusing to store it means the handle later resolves to
    /// nothing, which `AssimilateFactTool` already treats as `AgentInferred`
    /// (memory only, never the ledger).
    pub fn record(&self, handle: impl Into<String>, source: FactSource) {
        if let FactSource::WebSourced { url, .. } = &source {
            if url.trim().is_empty() {
                return;
            }
        }
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(handle.into(), source);
        }
    }

    /// The provenance recorded for `handle`, if any.
    #[must_use]
    pub fn resolve(&self, handle: &str) -> Option<FactSource> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(handle).cloned())
    }
}

impl ProvenanceSink for ProvenanceIndex {
    fn record_provenance(&self, handle: &str, source: FactSource) {
        self.record(handle, source);
    }
}

/// The `assimilate_fact` tool.
pub struct AssimilateFactTool {
    store: Arc<dyn VectorStore>,
    ledger: PendingFactsLedger,
    provenance: Arc<ProvenanceIndex>,
    approvals: Arc<KnowledgeApprovals>,
    scope: SessionScope,
}

impl AssimilateFactTool {
    /// Wires the tool to the session's memory store, ledger, provenance index
    /// and approval observations.
    #[must_use]
    pub fn new(
        store: Arc<dyn VectorStore>,
        ledger: PendingFactsLedger,
        provenance: Arc<ProvenanceIndex>,
        approvals: Arc<KnowledgeApprovals>,
    ) -> Self {
        Self {
            store,
            ledger,
            provenance,
            approvals,
            scope: SessionScope::new(),
        }
    }

    /// Shares the session's [`SessionScope`] with this tool.
    ///
    /// Pass the same scope the session's `semantic_memory` tool was built with:
    /// records this tool confines to the session are recalled only by that
    /// tool. Without an explicit scope each tool gets its own, which is
    /// fail-closed - the record is written and simply never recalled.
    #[must_use]
    pub fn with_session_scope(mut self, scope: SessionScope) -> Self {
        self.scope = scope;
        self
    }

    /// Decides whether a fact with this provenance may become durable, and
    /// spends whatever human approval that decision rests on.
    ///
    /// Returns `Ok(())` when it may, or `Err(reason)` explaining the refusal in
    /// terms the model can act on. An `Ok` for a source that needed an approval
    /// has already consumed it, so a later failure to append loses the
    /// approval - the safe direction: the human is asked again rather than the
    /// grant lingering for the next fact.
    fn admit(&self, source: &FactSource) -> Result<(), String> {
        match source.ledger_admission() {
            LedgerAdmission::Admissible => Ok(()),
            LedgerAdmission::RequiresIngestedDocument(digest) => {
                let ingested = self
                    .ledger
                    .ingested_document_digests()
                    .map_err(|e| format!("cannot read the pending-facts ledger: {e}"))?;
                if ingested.contains(&digest) {
                    Ok(())
                } else {
                    Err(format!(
                        "no document with digest {digest} was ever handed over; \
                         ingest the document first"
                    ))
                }
            }
            LedgerAdmission::RequiresHumanApproval => {
                // Spent, not merely observed: one human act admits one
                // agent-initiated fact. Otherwise the first approval would
                // ungate every page fetched after it for the rest of the
                // session, including ones no human ever saw.
                if self.approvals.consume_approval() {
                    Ok(())
                } else {
                    Err("web-sourced content needs a human approval before it can \
                         become durable knowledge, and each approval covers one fact"
                        .to_string())
                }
            }
            LedgerAdmission::Never => {
                Err("inferred facts stay in session memory and are never recorded \
                     for training"
                    .to_string())
            }
        }
    }
}

#[async_trait]
impl Tool for AssimilateFactTool {
    fn name(&self) -> &str {
        "assimilate_fact"
    }

    fn description(&self) -> &str {
        "Learn one fact: store it in semantic memory and, when its origin allows, \
         record it as durable knowledge.\n\
         Call it once per self-contained fact. Pass the `evidence` handle of \
         whatever established the fact (a fetched page, an ingested document, an \
         answered question); without one the fact is treated as your own inference \
         and is kept for this session only. You cannot declare where a fact came \
         from or that a human approved it - both are established outside this call."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "fact": {
                    "type": "string",
                    "description": "One self-contained assertion, in plain language. \
                                    Exactly one of 'fact'/'facts'."
                },
                "evidence": {
                    "type": "string",
                    "description": "Handle of the resolved evidence this fact (or, for \
                                    'facts', every fact in the batch) comes from, as \
                                    reported by the tool that resolved it"
                },
                "entity": {
                    "type": "string",
                    "description": "Person, component, or topic this fact is about"
                },
                "probe_question": {
                    "type": "string",
                    "description": "A question that can only be answered by knowing \
                                    this fact, written now, while you have the source \
                                    in front of you. Its wording must not appear in \
                                    the fact itself. Requires `expected_answer`"
                },
                "expected_answer": {
                    "type": "string",
                    "description": "The exact answer `probe_question` must elicit, as \
                                    short as it can be and still be correct"
                },
                "facts": {
                    "type": "array",
                    "description": "A batch of facts sharing one 'evidence' handle - \
                                    for extracting many facts from one already-ingested \
                                    document without a call per fact. Exactly one of \
                                    'fact'/'facts'. Each item takes the same \
                                    'fact'/'entity'/'probe_question'/'expected_answer' \
                                    shape as the top level; per-fact gating still \
                                    applies to every item individually.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "fact": {"type": "string"},
                            "entity": {"type": "string"},
                            "probe_question": {"type": "string"},
                            "expected_answer": {"type": "string"}
                        },
                        "required": ["fact"],
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::AssimilateKnowledge
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let single = call.args.get("fact").is_some();
        let batch = call.args.get("facts").is_some();

        // Provenance is resolved once, from the shared 'evidence' handle at
        // the top level - never asserted, and never re-read per batch item
        // (there is nothing per-item to read: a `source` field on a batch
        // entry would not be looked at either). An unknown handle is not an
        // error - it just makes every fact in this call the agent's own
        // inference.
        let source = call
            .args
            .get("evidence")
            .and_then(|v| v.as_str())
            .and_then(|handle| self.provenance.resolve(handle))
            .unwrap_or(FactSource::AgentInferred { from: Vec::new() });

        match (single, batch) {
            (true, true) => {
                ToolOutput::err(&call.id, "assimilate_fact takes exactly one of 'fact'/'facts', not both")
            }
            (false, false) => {
                ToolOutput::err(&call.id, "assimilate_fact requires either 'fact' or 'facts'")
            }
            (true, false) => {
                let (message, is_error) = self.assimilate_one(&call.args, &source).await;
                if is_error {
                    ToolOutput::err(&call.id, message)
                } else {
                    ToolOutput::ok(&call.id, message)
                }
            }
            (false, true) => {
                let items = match call.args.get("facts").and_then(Value::as_array) {
                    Some(items) if !items.is_empty() => items,
                    _ => return ToolOutput::err(&call.id, "'facts' must be a non-empty array"),
                };
                let mut lines = Vec::with_capacity(items.len());
                let mut any_ok = false;
                for (idx, item) in items.iter().enumerate() {
                    let (message, is_error) = self.assimilate_one(item, &source).await;
                    any_ok |= !is_error;
                    lines.push(format!("{}. {message}", idx + 1));
                }
                let summary = lines.join("\n");
                if any_ok {
                    ToolOutput::ok(&call.id, summary)
                } else {
                    ToolOutput::err(&call.id, summary)
                }
            }
        }
    }
}

impl AssimilateFactTool {
    /// Assimilates one fact from `args` (either the call's own top-level
    /// arguments, for a single-fact call, or one entry of `facts`, for a
    /// batch call), against the already-resolved `source`. Returns the
    /// human-facing result message and whether it represents a failure.
    ///
    /// This is the entire single-fact body `execute` used to be, extracted
    /// unchanged so a batch call runs it N times rather than duplicating it -
    /// per-fact gating (probe validation, admissibility, approval spending)
    /// is identical either way.
    async fn assimilate_one(&self, args: &Value, source: &FactSource) -> (String, bool) {
        let fact = match args.get("fact").and_then(|v| v.as_str()) {
            Some(f) if !f.trim().is_empty() => f.trim().to_string(),
            _ => return ("assimilate_fact requires a non-empty 'fact'".to_string(), true),
        };

        // Rejected before anything is written, memory included: a malformed
        // probe is a defect in the extraction the caller can fix and retry,
        // and remembering half of it would leave the ledger's scoring story
        // silently incomplete.
        let probe = match frozen_probe(args, &fact) {
            Ok(probe) => probe,
            Err(reason) => return (reason, true),
        };

        // Admissibility is decided *before* the memory write because it also
        // decides how the memory record is stamped. It spends any approval it
        // rests on, so a memory write that then fails loses that approval - the
        // same safe direction a failed ledger append already takes: the human
        // is asked again rather than the grant lingering for the next fact.
        // Re-evaluated per fact even within a batch sharing one `source` value:
        // a `RequiresHumanApproval` source still spends one approval per fact,
        // exactly as a batch of single calls would have.
        let admission = self.admit(source);

        let mut metadata = HashMap::new();
        // Not `source`: that key is free text the model can set through
        // `semantic_memory`'s `remember` action, and the recall path's trust
        // decision must never read a field the model can write.
        metadata.insert(PROVENANCE_KEY.to_string(), source.label().to_string());
        if let Some(entity) = args.get("entity").and_then(|v| v.as_str()) {
            metadata.insert("entity".to_string(), entity.to_string());
        }
        let recorded_at = chrono::Utc::now().timestamp().max(0) as u64;
        metadata.insert("created".to_string(), recorded_at.to_string());

        // A web record no human approved has no human act behind it at all, and
        // semantic memory is durable and shared by every session on the
        // machine. Confining it to this session is what stops one poisoned page
        // becoming a permanent injection channel into unrelated conversations;
        // an approved one is durable, which is exactly what the approval buys.
        // Nothing else is confined: a `UserProvidedDocument` refusal is a
        // forged digest the agent should still be able to reason about here,
        // and inferences are the agent's own working notes.
        if matches!(source, FactSource::WebSourced { .. }) && admission.is_err() {
            metadata.insert(
                SESSION_SCOPE_KEY.to_string(),
                self.scope.as_str().to_string(),
            );
        }

        // Always remembered for this session; only the durable half is gated.
        let doc_id = match self
            .store
            .insert(Document {
                content: fact.clone(),
                metadata,
                embedding: None,
            })
            .await
        {
            Ok(id) => id,
            Err(e) => return (format!("could not remember fact: {e}"), true),
        };

        if let Err(reason) = admission {
            return (
                format!(
                    "Remembered for this session (ID={doc_id}); not recorded as durable \
                     knowledge: {reason}"
                ),
                false,
            );
        }

        let record = PendingFactRecord {
            id: FactId::new(uuid::Uuid::new_v4().to_string()),
            fact,
            probe,
            source: source.clone(),
            recorded_at,
        };
        let ledger = self.ledger.clone();
        // The ledger does blocking filesystem I/O under an advisory lock.
        let written = tokio::task::spawn_blocking(move || ledger.record_fact(&record)).await;
        match written {
            Ok(Ok(())) => (format!("Remembered (ID={doc_id}) and recorded as durable knowledge."), false),
            Ok(Err(e)) => (format!("Remembered (ID={doc_id}), but the ledger append failed: {e}"), true),
            Err(e) => (format!("Remembered (ID={doc_id}), but the ledger append panicked: {e}"), true),
        }
    }
}

impl ToolDisplay for AssimilateFactTool {
    fn display_name(&self) -> &str {
        "AssimilateFact"
    }
    fn icon(&self) -> &str {
        "🎓"
    }
    fn category(&self) -> &str {
        "memory"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        if let Some(items) = args.get("facts").and_then(Value::as_array) {
            return format!("{} facts", items.len());
        }
        let fact = args.get("fact").and_then(|v| v.as_str()).unwrap_or("");
        if fact.chars().count() <= 60 {
            fact.to_string()
        } else {
            format!("{}...", fact.chars().take(60).collect::<String>())
        }
    }
}
