// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for measuring what an agent's
// memory really retrieves for its clients. If your team needs expertise in
// evaluating hybrid lexical and semantic retrieval then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Retrieval quality of the memory store over a directory of markdown, for
//! word-only search, the bundled hashing embedder and a served embedding model.
//!
//! ```text
//! cargo run --release -p sven-memory --example retrieval_quality -- \
//!     --docs docs --queries crates/memory/examples/retrieval_quality_queries.json \
//!     [--embed-url http://127.0.0.1:8788 --embed-model Qwen/Qwen3-Embedding-0.6B]
//! ```
//!
//! Every markdown file under `--docs` becomes one source cut into units by the
//! deterministic segmenter. A query is answered when the top `k` units include
//! one cut from the file the query is labelled with. The labels are written by
//! hand (a query paraphrases a document's subject without its headline words),
//! so the numbers compare modes on one corpus; they are not a benchmark.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sven_memory::HashingEmbedder;
use sven_memory_api::{
    AccessContext, Embedder, GenerationState, IngestRequest, MemoryStore, Origin, Permission,
    Provenance, RetrievalRequest, SourceId, SpaceId, TrainingUse, UnitId,
};
use sven_memory_sqlite::SqliteMemory;
use sven_model_drivers::OpenAiEmbedder;
use sven_sources::{segment, SourceKind};

const KS: [usize; 3] = [1, 3, 10];

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "adr") {
                continue;
            }
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

type Error = Box<dyn std::error::Error>;

/// Builds a store holding `files`, with vectors from `embedder` when given.
async fn build(
    files: &[(String, String)],
    embedder: Option<&Arc<dyn Embedder>>,
) -> Result<(SqliteMemory, AccessContext, SpaceId, Option<sven_memory_api::GenerationId>), Error> {
    let store = SqliteMemory::open_in_memory()?;
    let space = SpaceId::new("docs");
    let access = AccessContext::new("eval").writing(
        space.clone(),
        &[Permission::Ingest, Permission::Delete],
    );
    let generation = match embedder {
        Some(e) => {
            let g = store.register_generation(e.spec(), "eval-md").await?.generation_id;
            store.set_generation_state(&g, GenerationState::Active).await?;
            Some(g)
        }
        None => None,
    };
    for (name, text) in files {
        let units = segment(text, SourceKind::Markdown);
        let texts: Vec<String> = units.iter().map(|u| u.text.clone()).collect();
        let outcome = store
            .ingest(
                &access,
                IngestRequest {
                    space: space.clone(),
                    source_id: SourceId::new(name),
                    label: name.clone(),
                    text: text.clone(),
                    tags: BTreeSet::new(),
                    provenance: Provenance {
                        origin: Origin::Import {
                            format: "markdown".into(),
                        },
                        captured_at: 0,
                        training_use: TrainingUse::Unspecified,
                    },
                    units,
                },
            )
            .await?;
        if let (Some(e), Some(g)) = (embedder, &generation) {
            let ids: Vec<UnitId> = outcome.units().to_vec();
            // One request per source keeps a long input within the server's limits.
            for (ids, texts) in ids.chunks(16).zip(texts.chunks(16)) {
                let vectors = e.embed(texts).await?;
                store
                    .put_embeddings(&access, &space, g, ids.iter().cloned().zip(vectors).collect())
                    .await?;
            }
        }
    }
    Ok((store, access, space, generation))
}

async fn evaluate(
    label: &str,
    files: &[(String, String)],
    queries: &[(String, String)],
    embedder: Option<Arc<dyn Embedder>>,
) -> Result<(), Error> {
    let started = std::time::Instant::now();
    let (store, access, space, generation) = build(files, embedder.as_ref()).await?;
    let mut hits = [0usize; KS.len()];
    let mut reciprocal = 0.0;
    for (query, doc) in queries {
        let mut request = RetrievalRequest::new(query.clone(), 10);
        request.spaces = Some(BTreeSet::from([space.clone()]));
        if let (Some(e), Some(g)) = (&embedder, &generation) {
            let vector = e.embed(&[query.clone()]).await?.remove(0);
            request = request.with_embedding(g.clone(), vector);
        }
        let response = store.retrieve(&access, request).await?;
        let rank = response
            .hits
            .iter()
            .position(|h| h.unit.source_id.as_str() == doc);
        for (slot, k) in hits.iter_mut().zip(KS) {
            *slot += usize::from(rank.is_some_and(|r| r < k));
        }
        reciprocal += rank.map_or(0.0, |r| 1.0 / (r as f64 + 1.0));
    }
    let n = queries.len() as f64;
    println!(
        "{label:<28} hit@1 {:.2}  hit@3 {:.2}  hit@10 {:.2}  mrr {:.3}  ({} queries, {:.1}s)",
        hits[0] as f64 / n,
        hits[1] as f64 / n,
        hits[2] as f64 / n,
        reciprocal / n,
        queries.len(),
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let docs = PathBuf::from(arg("--docs").ok_or("--docs DIR is required")?);
    let queries_path = arg("--queries").ok_or("--queries FILE is required")?;
    let mut paths = Vec::new();
    markdown_files(&docs, &mut paths);
    paths.sort();
    let root = docs.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let files: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            let name = p.strip_prefix(&root).unwrap_or(p).display().to_string();
            Ok((name, std::fs::read_to_string(p)?))
        })
        .collect::<Result<_, Error>>()?;
    let raw: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(queries_path)?)?;
    let queries: Vec<(String, String)> = raw
        .iter()
        .filter_map(|q| Some((q["query"].as_str()?.to_owned(), q["doc"].as_str()?.to_owned())))
        .filter(|(_, doc)| files.iter().any(|(name, _)| name == doc))
        .collect();
    println!("{} documents, {} labelled queries", files.len(), queries.len());

    evaluate("words only (BM25)", &files, &queries, None).await?;
    let hashing: Arc<dyn Embedder> = Arc::new(HashingEmbedder::default());
    evaluate("words + hashing embedder", &files, &queries, Some(hashing)).await?;
    if let Some(url) = arg("--embed-url") {
        let model = arg("--embed-model").ok_or("--embed-model is required with --embed-url")?;
        let served: Arc<dyn Embedder> =
            Arc::new(OpenAiEmbedder::connect(&url, arg("--api-key"), &model).await?);
        evaluate(&format!("words + {model}"), &files, &queries, Some(served)).await?;
    }
    Ok(())
}
