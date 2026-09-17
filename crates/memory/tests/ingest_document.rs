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
use sven_tools::policy::ApprovalPolicy;
use sven_tools::tool::{Tool, ToolCall};
use sven_tools::ToolCapability;
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

    assert!(
        !out.is_error,
        "ingest_document should succeed: {}",
        out.content
    );

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
            assert_eq!(
                digest, real,
                "attached provenance must carry the real digest too"
            );
            assert_eq!(uri, file_path.to_string_lossy());
        }
        other => panic!("expected UserProvidedDocument provenance, got {other:?}"),
    }
}

/// Neither `path` nor `url` is a descriptive error, not a panic or a silent
/// no-op.
#[tokio::test]
async fn ingest_document_requires_a_path_or_url() {
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
        "neither path nor url means nothing should ever have been recorded"
    );
}

/// Giving both is refused, not silently resolved by picking one - an
/// ambiguous call should never guess which artifact the human meant.
#[tokio::test]
async fn ingest_document_refuses_both_path_and_url_together() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, b"content").expect("write fixture file");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    let tool = IngestDocumentTool::new(ledger.clone());

    let out = tool
        .execute(&ToolCall {
            id: "call-1".to_string(),
            name: "ingest_document".to_string(),
            args: json!({
                "path": file_path.to_string_lossy(),
                "url": "http://127.0.0.1:1/unreachable",
            }),
        })
        .await;

    assert!(out.is_error);
    assert!(
        ledger
            .ingested_document_digests()
            .expect("read ledger")
            .is_empty(),
        "an ambiguous call must record nothing"
    );
}

/// Starts a one-shot local HTTP/1.1 server on an ephemeral port, replies once
/// with `body` (as `text/plain`), and returns the URL to fetch it from. No
/// mocking of `reqwest` itself - `fetch_document` hits a real socket, the same
/// discipline `sven-executors::verify`'s `HttpPredicate` tests use.
async fn serve_once(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}/")
}

/// The URL path: no local file at all, yet the ingested digest is real (over
/// the fetched bytes) and the returned message carries the fetched text
/// directly, since there is no file for the model to re-read afterward.
#[tokio::test]
async fn ingest_document_from_a_url_fetches_and_returns_readable_text() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    let tool = IngestDocumentTool::new(ledger.clone());
    let url = serve_once("The CAN bus runs at 500 kbit/s.").await;

    let out = tool
        .execute(&ToolCall {
            id: "call-1".to_string(),
            name: "ingest_document".to_string(),
            args: json!({"url": url}),
        })
        .await;

    assert!(
        !out.is_error,
        "ingest_document should succeed: {}",
        out.content
    );
    assert!(
        out.content.contains("The CAN bus runs at 500 kbit/s."),
        "the fetched text must be in the tool's own output, since there is no \
         local file for the model to re-read: {}",
        out.content
    );

    let real = real_digest(b"The CAN bus runs at 500 kbit/s.");
    let ingested = ledger.ingested_document_digests().expect("read ledger");
    assert_eq!(ingested, HashSet::from([real.clone()]));

    match out.provenance.map(|b| *b) {
        Some(FactSource::UserProvidedDocument { digest, uri, .. }) => {
            assert_eq!(digest, real);
            assert_eq!(uri, url);
        }
        other => panic!("expected UserProvidedDocument provenance, got {other:?}"),
    }
}

/// The whole document half of `assimilate_fact`'s gate rests on one claim:
/// that a human handed this artifact over, which is why a fact extracted from
/// an ingested digest is admissible with no per-fact approval at all.
///
/// Nothing about `ingest_document` establishes that claim on its own. It is a
/// tool the *model* calls, on a path the *model* chooses, and every capability
/// it needs to manufacture its own evidence is granted by default in the agent
/// mode: fetch a page (`NetworkAccess`), write it to a file (`WriteFile`),
/// ingest that file, then assimilate facts from it citing the ingest call as
/// evidence. That is the laundering path S4's `RequiresHumanApproval` rule
/// exists to close, walked end to end with no human act anywhere in it - and an
/// injected page is exactly what steers an agent through it.
///
/// So the human act has to be real, and the only channel in this architecture
/// that produces one is the kernel's approval gate. This pins the tool's half
/// of that: the capability it declares must be one no policy can wave through,
/// and the unattended entry points must refuse it rather than run it.
#[tokio::test]
async fn ingesting_a_document_always_costs_a_granted_human_approval() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let ledger = PendingFactsLedger::new(dir.path().join("pending-facts.jsonl"));
    let tool = IngestDocumentTool::new(ledger);

    assert_eq!(
        tool.kernel_capability(),
        ToolCapability::IngestDocument,
        "declaring an artifact handed-over is its own act, not a file read"
    );
    assert!(
        tool.kernel_capability().is_inherently_dangerous(),
        "recording a document as handed-over must always require a granted \
         human approval, whatever the mode's allow-set says"
    );
    assert_eq!(
        tool.default_policy(),
        ApprovalPolicy::Ask,
        "an unattended entry point with no requester must deny the ingest, \
         not run it"
    );
}
