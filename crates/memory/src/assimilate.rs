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
//! * human confirmation is read from [`KnowledgeApprovals`], which only the
//!   effect executor that observed a real `HumanApproved` event writes - a
//!   `confirmed` field in the tool-call arguments is not read at all.
//!
//! The fact is *always* written to semantic memory (instant recall this
//! session). Only the durable ledger - what later feeds training - is gated.
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
use sven_vocab::provenance::{FactId, FactSource, KnowledgeApprovals, LedgerAdmission};

use crate::{
    ledger::{PendingFactRecord, PendingFactsLedger},
    store::{Document, VectorStore},
};

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
    pub fn record(&self, handle: impl Into<String>, source: FactSource) {
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

/// The `assimilate_fact` tool.
pub struct AssimilateFactTool {
    store: Arc<dyn VectorStore>,
    ledger: PendingFactsLedger,
    provenance: Arc<ProvenanceIndex>,
    approvals: Arc<KnowledgeApprovals>,
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
        }
    }

    /// Decides whether a fact with this provenance may become durable.
    ///
    /// Returns `Ok(())` when it may, or `Err(reason)` explaining the refusal in
    /// terms the model can act on.
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
                if self.approvals.human_approved() {
                    Ok(())
                } else {
                    Err("web-sourced content needs a human approval before it can \
                         become durable knowledge"
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
                    "description": "One self-contained assertion, in plain language"
                },
                "evidence": {
                    "type": "string",
                    "description": "Handle of the resolved evidence this fact comes \
                                    from, as reported by the tool that resolved it"
                },
                "entity": {
                    "type": "string",
                    "description": "Person, component, or topic this fact is about"
                }
            },
            "required": ["fact"],
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
        let fact = match call.args.get("fact").and_then(|v| v.as_str()) {
            Some(f) if !f.trim().is_empty() => f.trim().to_string(),
            _ => return ToolOutput::err(&call.id, "assimilate_fact requires a non-empty 'fact'"),
        };

        // Provenance is resolved, never asserted: a `source` (or `confirmed`)
        // field in `call.args` is deliberately not read. An unknown handle is
        // not an error - it just makes the fact the agent's own inference.
        let source = call
            .args
            .get("evidence")
            .and_then(|v| v.as_str())
            .and_then(|handle| self.provenance.resolve(handle))
            .unwrap_or(FactSource::AgentInferred { from: Vec::new() });

        let mut metadata = HashMap::new();
        metadata.insert("source".to_string(), source.label().to_string());
        if let Some(entity) = call.args.get("entity").and_then(|v| v.as_str()) {
            metadata.insert("entity".to_string(), entity.to_string());
        }
        let recorded_at = chrono::Utc::now().timestamp().max(0) as u64;
        metadata.insert("created".to_string(), recorded_at.to_string());

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
            Err(e) => return ToolOutput::err(&call.id, format!("could not remember fact: {e}")),
        };

        if let Err(reason) = self.admit(&source) {
            return ToolOutput::ok(
                &call.id,
                format!(
                    "Remembered for this session (ID={doc_id}); not recorded as durable \
                     knowledge: {reason}"
                ),
            );
        }

        let record = PendingFactRecord {
            id: FactId::new(uuid::Uuid::new_v4().to_string()),
            fact,
            source,
            recorded_at,
        };
        let ledger = self.ledger.clone();
        // The ledger does blocking filesystem I/O under an advisory lock.
        let written = tokio::task::spawn_blocking(move || ledger.record_fact(&record)).await;
        match written {
            Ok(Ok(())) => ToolOutput::ok(
                &call.id,
                format!("Remembered (ID={doc_id}) and recorded as durable knowledge."),
            ),
            Ok(Err(e)) => ToolOutput::err(
                &call.id,
                format!("Remembered (ID={doc_id}), but the ledger append failed: {e}"),
            ),
            Err(e) => ToolOutput::err(
                &call.id,
                format!("Remembered (ID={doc_id}), but the ledger append panicked: {e}"),
            ),
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
        let fact = args.get("fact").and_then(|v| v.as_str()).unwrap_or("");
        if fact.chars().count() <= 60 {
            fact.to_string()
        } else {
            format!("{}...", fact.chars().take(60).collect::<String>())
        }
    }
}
