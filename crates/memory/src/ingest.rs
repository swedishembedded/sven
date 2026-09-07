// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `ingest_document` - the entry point for learning from a document the user
//! hands over.
//!
//! Reads the file at the given path and computes its digest over the bytes it
//! actually read - never a model-supplied claim, closing the same
//! "model-supplied field" hole `assimilate_fact`'s own tests guard against -
//! then records a [`DocumentRecord`] in the pending-facts ledger. That digest
//! is exactly what `assimilate_fact`'s `FactSource::UserProvidedDocument`
//! admissibility check matches against: the human act of handing over *this*
//! artifact is the approval, and it covers every fact later extracted from
//! it, with no per-fact confirmation.
//!
//! This tool never extracts facts itself - extraction happens afterwards,
//! inside the existing tool loop, via repeated `assimilate_fact` calls citing
//! this call's own id as their `evidence`. To make that resolvable, this
//! tool's own [`ToolOutput`] also carries `FactSource::UserProvidedDocument`
//! provenance, the same pattern `web_fetch`/`web_search` use for `WebSourced`.
//!
//! Swedish Embedded AB implements solutions for provenance-tracked document
//! ingestion in autonomous agents for its clients. If your team needs
//! expertise in trustworthy agent knowledge capture then you can procure our
//! services by sending an email to info@swedishembedded.com.

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
    ToolCapability,
};
use sven_vocab::provenance::{ContentDigest, FactSource};

use crate::ledger::{DocumentRecord, PendingFactsLedger};

/// The `ingest_document` tool.
pub struct IngestDocumentTool {
    ledger: PendingFactsLedger,
}

impl IngestDocumentTool {
    /// Wires the tool to the session's pending-facts ledger.
    #[must_use]
    pub fn new(ledger: PendingFactsLedger) -> Self {
        Self { ledger }
    }
}

#[async_trait]
impl Tool for IngestDocumentTool {
    fn name(&self) -> &str {
        "ingest_document"
    }

    fn description(&self) -> &str {
        "Record that the user handed over a document to learn from. Reads the \
         file at 'path' and computes its digest yourself - you cannot set the \
         digest. After this call, extract facts from the document's content \
         with repeated assimilate_fact calls, passing this call's own id as \
         'evidence' each time: each such fact is admissible as durable \
         knowledge, because a human handed over this exact artifact."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file the user handed over"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            _ => return ToolOutput::err(&call.id, "ingest_document requires a non-empty 'path'"),
        };

        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(&call.id, format!("cannot read {path}: {e}")),
        };

        // Computed here, over the bytes actually read - a model-supplied
        // 'digest' argument, if any, is not even read.
        let digest = content_digest(&bytes);
        let ingested_at = now_unix();
        let record = DocumentRecord {
            digest: digest.clone(),
            uri: path.clone(),
            ingested_at,
        };

        let ledger = self.ledger.clone();
        // The ledger does blocking filesystem I/O under an advisory lock.
        let written = tokio::task::spawn_blocking(move || ledger.record_document(&record)).await;

        match written {
            Ok(Ok(())) => ToolOutput::ok(
                &call.id,
                format!(
                    "Ingested {path} ({} bytes, digest={digest}). Extract facts from its \
                     content with assimilate_fact, passing evidence=\"{}\" each time.",
                    bytes.len(),
                    call.id
                ),
            )
            .with_provenance(FactSource::UserProvidedDocument {
                digest,
                uri: path,
                ingested_at,
                span: 0..bytes.len(),
            }),
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("could not record document: {e}")),
            Err(e) => ToolOutput::err(&call.id, format!("ledger append panicked: {e}")),
        }
    }
}

impl ToolDisplay for IngestDocumentTool {
    fn display_name(&self) -> &str {
        "IngestDocument"
    }
    fn icon(&self) -> &str {
        "📄"
    }
    fn category(&self) -> &str {
        "memory"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string()
    }
}

/// Hex-encoded SHA-256 of `bytes`.
fn content_digest(bytes: &[u8]) -> ContentDigest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    ContentDigest::from_hex(hex::encode(hasher.finalize()))
}

/// Current Unix time in seconds, saturating at 0 rather than panicking on a
/// clock set before the epoch.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
