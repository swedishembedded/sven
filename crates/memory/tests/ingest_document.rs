// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `ingest_document` computes its own digest.
//!
//! The digest recorded in the pending-facts ledger - the value
//! `assimilate_fact`'s `UserProvidedDocument` admissibility check matches
//! against - must always be the one the tool computed over the bytes it
//! actually read from disk, never a value the model supplies in its own
//! tool-call arguments. A model that could set its own digest could launder
//! arbitrary content as "the document the human handed over".
//!
//! Swedish Embedded AB implements solutions for provenance-tracked document
//! ingestion in autonomous agents for its clients. If your team needs
//! expertise in trustworthy agent knowledge capture then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::HashSet;

use serde_json::json;
use sha2::{Digest, Sha256};

use sven_memory::{IngestDocumentTool, PendingFactsLedger};
use sven_tools::tool::{Tool, ToolCall};
use sven_vocab::provenance::{ContentDigest, FactSource};

fn real_digest(bytes: &[u8]) -> ContentDigest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    ContentDigest::from_hex(hex::encode(hasher.finalize()))
}

/// The actual security boundary: a `digest` field in the tool call's own
/// arguments is a suggestion, not a source of truth. The ledger - and the
/// provenance attached to this tool's own `ToolOutput`, which a later
/// `assimilate_fact` call resolves by citing this call's id as `evidence` -
/// must both carry the digest computed from the real file bytes.
#[tokio::test]
async fn ingest_document_computes_its_own_digest_never_trusts_a_model_supplied_one() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file_path = dir.path().join("spec.md");
    let content = b"The CAN bus runs at 500 kbit/s.";
    std::fs::write(&file_path, content).expect("write fixture file");

    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    let tool = IngestDocumentTool::new(ledger.clone());

    let out = tool
        .execute(&ToolCall {
            id: "call-1".to_string(),
            name: "ingest_document".to_string(),
            args: json!({
                "path": file_path.to_string_lossy(),
                // Never even read - closing the same "model-supplied field"
                // hole assimilate_fact's own tests guard against.
                "digest": "0000000000deadbeef",
            }),
        })
        .await;

    assert!(!out.is_error, "ingest_document should succeed: {}", out.content);

    let real = real_digest(content);
    let ingested = ledger.ingested_document_digests().expect("read ledger");
    assert_eq!(
        ingested,
        HashSet::from([real.clone()]),
        "the ledger must hold the digest computed from the file's real bytes, \
         never the model-supplied one"
    );

    match out.provenance.map(|b| *b) {
        Some(FactSource::UserProvidedDocument { digest, uri, .. }) => {
            assert_eq!(digest, real, "attached provenance must carry the real digest too");
            assert_eq!(uri, file_path.to_string_lossy());
        }
        other => panic!("expected UserProvidedDocument provenance, got {other:?}"),
    }
}

/// A missing `path` is a descriptive error, not a panic or a silent no-op.
#[tokio::test]
async fn ingest_document_requires_a_path() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    let tool = IngestDocumentTool::new(ledger.clone());

    let out = tool
        .execute(&ToolCall {
            id: "call-1".to_string(),
            name: "ingest_document".to_string(),
            args: json!({}),
        })
        .await;

    assert!(out.is_error);
    assert!(
        ledger
            .ingested_document_digests()
            .expect("read ledger")
            .is_empty(),
        "no path means nothing should ever have been recorded"
    );
}
