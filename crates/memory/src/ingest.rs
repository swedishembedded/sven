// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `ingest_document` - the entry point for learning from a document the user
//! hands over, whether that document is a local file or a URL.
//!
//! Reads the file (or fetches the URL) and computes its digest over the bytes
//! actually read - never a model-supplied claim, closing the same
//! "model-supplied field" hole `assimilate_fact`'s own tests guard against -
//! then records a [`DocumentRecord`] in the pending-facts ledger. That digest
//! is exactly what `assimilate_fact`'s `FactSource::UserProvidedDocument`
//! admissibility check matches against: the human act of handing over *this*
//! artifact is the approval, and it covers every fact later extracted from
//! it, with no per-fact confirmation.
//!
//! # Why a URL is handled here rather than via `web_fetch` + `ingest_document`
//!
//! `web_fetch` mints `FactSource::WebSourced`, admissible only per fact and
//! only after a human approval spent on that one fact - fine for a page the
//! agent found on its own, wrong for a document the operator explicitly named
//! on the command line (`sven task run`/a future `sven learn document <url>`).
//! Writing the fetched bytes to a temp file and then calling
//! `ingest_document(path=...)` would launder `WebSourced` into
//! `UserProvidedDocument` through a side door - exactly the hole
//! `ToolCapability::IngestDocument` being inherently dangerous exists to
//! close (see `kernel_capability`'s doc below). So the fetch happens *inside*
//! this same inherently-dangerous, human-approved call instead: one approval
//! for the whole document, honestly, because the human named the URL.
//!
//! This tool never extracts facts itself - extraction happens afterwards,
//! inside the existing tool loop, via repeated `assimilate_fact` calls citing
//! this call's own id as their `evidence`. For a local file the model is
//! expected to have already read it some other way; for a URL there is no
//! local file to re-read, so this call's own [`ToolOutput`] carries the
//! fetched, readable text directly (HTML rendered to text, truncated to
//! `max_chars`, mirroring `sven-tools-web::web_fetch`'s UX) - but the digest
//! is always computed over the raw bytes actually received, before any
//! conversion, so it stays a faithful fingerprint of what was fetched. To
//! make evidence resolvable, this tool's own [`ToolOutput`] also carries
//! `FactSource::UserProvidedDocument` provenance, the same pattern
//! `web_fetch`/`web_search` use for `WebSourced`.
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

/// Default cap on the readable text returned for a URL ingestion, matching
/// `sven-tools-web::web_fetch`'s own default.
const DEFAULT_MAX_CHARS: usize = 20_000;

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
        "Record that the user handed over a document to learn from - a local \
         file ('path') or a URL ('url'), exactly one of the two. The digest is \
         always computed over the bytes actually read or fetched; you cannot \
         set it yourself. For a local file, extract facts afterwards with \
         repeated assimilate_fact calls citing this call's own id as \
         'evidence'. For a URL, this call's own result already contains the \
         fetched, readable text - extract facts directly from it, still citing \
         this call's id as 'evidence'. Each such fact is admissible as durable \
         knowledge, because a human named this exact artifact."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file the user handed over. Exactly one of 'path'/'url'."
                },
                "url": {
                    "type": "string",
                    "description": "URL of the document the user handed over. Exactly one of 'path'/'url'."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Cap on the readable text returned for a URL ingestion (default 20000). Ignored for 'path'."
                }
            },
            "additionalProperties": false
        })
    }

    /// Ask, never auto.
    ///
    /// The kernel's [`ToolCapability::IngestDocument`] gate protects the
    /// surfaces that drive an HSM; this protects the ones that do not.
    /// `ToolRegistry::execute_with_requester` - the unattended entry point -
    /// denies an `Ask` tool outright when no requester is available, which is
    /// the right answer here: an ingest with nobody to ask is an ingest with
    /// no human behind it.
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    /// Not [`ToolCapability::ReadFile`].
    ///
    /// Reading the bytes is the incidental part; what this call does is
    /// *declare* the artifact handed-over, and that declaration is the entire
    /// basis on which `assimilate_fact` admits facts extracted from it to the
    /// pending-facts ledger with no per-fact approval. `ReadFile` is granted
    /// globally in every mode, so under it the agent can manufacture its own
    /// evidence - fetch a page, write it to a file, ingest that file - and walk
    /// untrusted content into training data with no human act anywhere.
    /// `IngestDocument` is inherently dangerous, so the kernel requires a real
    /// human approval first.
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::IngestDocument
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path = call.args.get("path").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
        let url = call.args.get("url").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());

        let (uri, bytes, readable_text) = match (path, url) {
            (Some(_), Some(_)) => {
                return ToolOutput::err(&call.id, "ingest_document takes exactly one of 'path'/'url', not both")
            }
            (None, None) => {
                return ToolOutput::err(&call.id, "ingest_document requires either 'path' or 'url'")
            }
            (Some(path), None) => {
                let bytes = match tokio::fs::read(path).await {
                    Ok(b) => b,
                    Err(e) => return ToolOutput::err(&call.id, format!("cannot read {path}: {e}")),
                };
                (path.to_string(), bytes, None)
            }
            (None, Some(url)) => {
                let max_chars = call
                    .args
                    .get("max_chars")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(DEFAULT_MAX_CHARS, |n| n as usize);
                match fetch_document(url, max_chars).await {
                    Ok((bytes, text)) => (url.to_string(), bytes, Some(text)),
                    Err(e) => return ToolOutput::err(&call.id, format!("fetching {url}: {e}")),
                }
            }
        };

        // Computed here, over the bytes actually read/fetched - a
        // model-supplied 'digest' argument, if any, is not even read.
        let digest = content_digest(&bytes);
        let ingested_at = now_unix();
        let record = DocumentRecord {
            digest: digest.clone(),
            uri: uri.clone(),
            ingested_at,
        };

        let ledger = self.ledger.clone();
        // The ledger does blocking filesystem I/O under an advisory lock.
        let written = tokio::task::spawn_blocking(move || ledger.record_document(&record)).await;

        let message = match &readable_text {
            None => format!(
                "Ingested {uri} ({} bytes, digest={digest}). Extract facts from its \
                 content with assimilate_fact, passing evidence=\"{}\" each time.",
                bytes.len(),
                call.id
            ),
            Some(text) => format!(
                "Ingested {uri} ({} bytes fetched, digest={digest}). Extract facts \
                 directly from the content below with assimilate_fact, passing \
                 evidence=\"{}\" each time.\n\n{text}",
                bytes.len(),
                call.id
            ),
        };

        match written {
            Ok(Ok(())) => ToolOutput::ok(&call.id, message).with_provenance(
                FactSource::UserProvidedDocument { digest, uri, ingested_at, span: 0..bytes.len() },
            ),
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
        args.get("path")
            .or_else(|| args.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }
}

/// Fetches `url` and returns `(raw bytes, readable text)` - the digest is
/// computed by the caller over the raw bytes, before this function's own HTML
/// rendering/truncation, so it stays a faithful fingerprint of what was
/// actually received. Same client configuration as
/// `sven-tools-web::web_fetch` (timeout, redirect limit, user agent), kept as
/// a separate implementation rather than a cross-crate call to avoid a
/// same-tier domain->domain dependency edge.
async fn fetch_document(url: &str, max_chars: usize) -> anyhow::Result<(Vec<u8>, String)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("sven-agent/0.1")
        .build()?;

    let response = client.get(url).send().await?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let bytes = response.bytes().await?.to_vec();
    let body = String::from_utf8_lossy(&bytes);

    let mut text = if content_type.contains("html") {
        html2text::from_read(body.as_bytes(), 100)
    } else {
        body.into_owned()
    };
    if text.len() > max_chars {
        let boundary = floor_char_boundary(&text, max_chars);
        text.truncate(boundary);
        text.push_str("\n... [truncated]");
    }
    Ok((bytes, text))
}

/// The largest byte offset `<= max` that lands on a UTF-8 character boundary.
/// `max` itself may be attacker/model-influenced (`max_chars`), so this never
/// assumes it already is one.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
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
