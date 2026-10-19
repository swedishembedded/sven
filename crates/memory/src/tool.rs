// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `semantic_memory` tool over a [`MemoryStore`].
//!
//! Each note is a source of one unit in the tool's space. The id the model
//! sees is the note's opaque source id, never a row number.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_memory_api::{
    quote_untrusted, AccessContext, Embedder, GenerationId, GenerationState, MemoryError,
    MemoryStore, Note, Origin, Permission, Provenance, RetrievalRecord, RetrievalRequest, SourceId,
    SourceSummary, SpaceId, TrainingUse, UnitId, ENTITY_TAG_PREFIX,
};
use sven_tool_api::{
    call_records::record_memory_retrieval,
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
    ToolCapability,
};

/// The space the CLI keeps an agent's notes in.
pub const PERSONAL_SPACE: &str = "personal";

/// What the trajectory records as the generation of a purely lexical query,
/// which reads no embedding index.
const LEXICAL_GENERATION: &str = "lexical";

/// Semantic memory tool backed by a [`MemoryStore`].
///
/// # Actions
///
/// - `remember` - store a fact, note, or contact detail
/// - `recall` - search for relevant memories
/// - `forget` - delete a memory by ID
/// - `list` - list stored memories
/// - `get` - retrieve a specific memory by ID
pub struct SemanticMemoryTool {
    store: Arc<dyn MemoryStore>,
    access: AccessContext,
    space: SpaceId,
    /// Names this tool instance as the author of what it writes.
    run_id: String,
    /// Embeds what is remembered and what is asked, when the host gave one,
    /// with the active generation of its model.
    semantic: Option<Semantic>,
}

/// An embedder and the index generation its vectors belong to.
struct Semantic {
    embedder: Arc<dyn Embedder>,
    generation: GenerationId,
}

/// The segmenter version the tool's notes are cut by, part of the identity of
/// the index generation its vectors belong to.
pub const NOTES_SEGMENTER: &str = "sven-segmenter-notes-1";

impl SemanticMemoryTool {
    /// A tool that reads and writes `space` of `store` as `access` allows.
    ///
    /// `access` should grant [`Permission::Ingest`] and [`Permission::Delete`]
    /// in `space` for `remember` and `forget` to work; without them those
    /// actions report the refusal.
    pub fn new(store: Arc<dyn MemoryStore>, access: AccessContext, space: SpaceId) -> Self {
        Self {
            store,
            access,
            space,
            run_id: uuid::Uuid::new_v4().to_string(),
            semantic: None,
        }
    }

    /// Makes the tool search by meaning as well as by word: what it remembers
    /// from now on is embedded with `embedder`, and a recall reads the vectors
    /// of the generation of that model, which is registered and activated here.
    /// Notes remembered before are found by word only until they are
    /// remembered again.
    ///
    /// # Errors
    /// The store's refusal to register or activate the generation.
    pub async fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Result<Self, MemoryError> {
        let generation = self
            .store
            .register_generation(embedder.spec(), NOTES_SEGMENTER)
            .await?
            .generation_id;
        self.store
            .set_generation_state(&generation, GenerationState::Active)
            .await?;
        self.semantic = Some(Semantic {
            embedder,
            generation,
        });
        Ok(self)
    }

    /// A tool over the [`PERSONAL_SPACE`] of `store`, with full rights in it
    /// and none anywhere else.
    pub fn personal(store: Arc<dyn MemoryStore>) -> Self {
        let space = SpaceId::new(PERSONAL_SPACE);
        let access = AccessContext::new("sven")
            .writing(space.clone(), &[Permission::Ingest, Permission::Delete]);
        Self::new(store, access, space)
    }
}

#[async_trait]
impl Tool for SemanticMemoryTool {
    fn name(&self) -> &str {
        "semantic_memory"
    }

    fn summary(&self) -> &str {
        "Store and recall facts and notes in persistent memory."
    }

    fn description(&self) -> &str {
        "Persistent memory: store and recall facts, notes, contacts, and action items across sessions.\n\
         Actions: remember | recall | forget | list | get\n\
         recall matches words, not meaning. IDs are opaque text: pass them back unchanged."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["remember", "recall", "forget", "list", "get"],
                    "description": "Memory operation to perform"
                },
                "content": {
                    "type": "string",
                    "description": "(remember) Text content to store"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "(remember) Tags for categorization"
                },
                "entity": {
                    "type": "string",
                    "description": "(remember) Person, company, or topic this memory is about"
                },
                "source": {
                    "type": "string",
                    "description": "(remember) Where it came from (e.g. 'email')"
                },
                "query": {
                    "type": "string",
                    "description": "(recall) Words to search memories for"
                },
                "limit": {
                    "type": "integer",
                    "description": "(recall/list) Maximum number of results. Default: 10 for recall, 20 for list"
                },
                "tag_filter": {
                    "type": "string",
                    "description": "(list) Only memories with exactly this tag or entity"
                },
                "id": {
                    "type": "string",
                    "description": "(forget/get) Memory ID"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::WriteFile
    }

    /// `remember` and `forget` change the store; the rest only read it.
    fn call_capability(&self, args: &Value) -> ToolCapability {
        sven_tool_api::capability_by_action(
            args,
            &[
                ("remember", ToolCapability::WriteFile),
                ("forget", ToolCapability::WriteFile),
                ("recall", ToolCapability::ReadFile),
                ("list", ToolCapability::ReadFile),
                ("get", ToolCapability::ReadFile),
            ],
            ToolCapability::WriteFile,
        )
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolOutput::err(&call.id, "missing 'action'"),
        };

        match action {
            "remember" => self.remember(call).await,
            "recall" => self.recall(call).await,
            "forget" => self.forget(call).await,
            "list" => self.list(call).await,
            "get" => self.get(call).await,
            other => ToolOutput::err(
                &call.id,
                format!("unknown action {other:?}; expected remember|recall|forget|list|get"),
            ),
        }
    }
}

impl SemanticMemoryTool {
    async fn remember(&self, call: &ToolCall) -> ToolOutput {
        let Some(content) = str_arg(call, "content").filter(|c| !c.trim().is_empty()) else {
            return ToolOutput::err(&call.id, "remember requires non-empty 'content'");
        };
        let id = SourceId::new(format!(
            "n-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        ));
        let tags = call
            .args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|tags| {
                tags.iter()
                    .filter_map(|t| t.as_str())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let note = Note {
            space: self.space.clone(),
            source_id: id.clone(),
            text: content,
            entity: str_arg(call, "entity"),
            source: str_arg(call, "source"),
            tags,
            provenance: Provenance {
                origin: Origin::Agent {
                    run_id: self.run_id.clone(),
                },
                captured_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
                training_use: TrainingUse::Unspecified,
            },
        };
        let request = note.into_request();
        let texts: Vec<String> = request.units.iter().map(|u| u.text.clone()).collect();
        match self.store.ingest(&self.access, request).await {
            Ok(outcome) => {
                if let Err(e) = self.embed_units(outcome.units(), &texts).await {
                    return ToolOutput::err(
                        &call.id,
                        format!("remembered (ID={id}) but not embedded, so it is found by word only: {e}"),
                    );
                }
                ToolOutput::ok(
                    &call.id,
                    format!("Remembered (ID={id}): {}", truncate(content, 80)),
                )
            }
            Err(e) => ToolOutput::err(&call.id, format!("remember failed: {e}")),
        }
    }

    async fn recall(&self, call: &ToolCall) -> ToolOutput {
        let Some(query) = str_arg(call, "query") else {
            return ToolOutput::err(&call.id, "recall requires 'query'");
        };
        let limit = usize_arg(call, "limit", 10);
        let mut request = RetrievalRequest::new(query, limit);
        if let Some(semantic) = &self.semantic {
            match semantic.embedder.embed(&[query.to_owned()]).await {
                Ok(mut vectors) if vectors.len() == 1 => {
                    request =
                        request.with_embedding(semantic.generation.clone(), vectors.remove(0));
                }
                Ok(_) => {
                    return ToolOutput::err(
                        &call.id,
                        "recall failed: the embedder answered the wrong number of vectors",
                    )
                }
                Err(e) => return ToolOutput::err(&call.id, format!("recall failed: {e}")),
            }
        }
        let response = match self.store.retrieve(&self.access, request.clone()).await {
            Ok(response) => response,
            Err(e) => return ToolOutput::err(&call.id, format!("recall failed: {e}")),
        };

        // Every hit is shown, so every hit was put into the context.
        record_memory_retrieval(to_trajectory(RetrievalRecord::new(
            &request,
            &response,
            |_| true,
        )));

        if response.hits.is_empty() {
            return ToolOutput::ok(&call.id, "No memories found for that query.");
        }
        let mut lines = vec![format!("Found {} memories:", response.hits.len())];
        for hit in &response.hits {
            lines.push(format!(
                "- ID={}: {}",
                hit.unit.source_id,
                framed(&hit.provenance.origin, &truncate(&hit.unit.text, 120))
            ));
        }
        ToolOutput::ok(&call.id, lines.join("\n"))
    }

    /// Stores the embeddings of freshly ingested units of the tool's space.
    async fn embed_units(&self, units: &[UnitId], texts: &[String]) -> Result<(), MemoryError> {
        let Some(semantic) = &self.semantic else {
            return Ok(());
        };
        let vectors = semantic.embedder.embed(texts).await?;
        if vectors.len() != units.len() {
            return Err(MemoryError::Unavailable(
                "the embedder answered the wrong number of vectors".into(),
            ));
        }
        self.store
            .put_embeddings(
                &self.access,
                &self.space,
                &semantic.generation,
                units.iter().cloned().zip(vectors).collect(),
            )
            .await
    }

    async fn forget(&self, call: &ToolCall) -> ToolOutput {
        let Some(id) = str_arg(call, "id") else {
            return ToolOutput::err(&call.id, "forget requires 'id'");
        };
        // A note of a space the tool cannot see answers exactly as a missing
        // one: the store reports it as absent, so the reply is no oracle for
        // whether it exists.
        match self
            .store
            .delete_source(&self.access, &self.space, &SourceId::new(id))
            .await
        {
            Ok(true) => ToolOutput::ok(&call.id, format!("Memory {id} deleted.")),
            Ok(false) | Err(MemoryError::NotFound) => {
                ToolOutput::err(&call.id, format!("No memory with ID {id}."))
            }
            Err(e) => ToolOutput::err(&call.id, format!("forget failed: {e}")),
        }
    }

    async fn list(&self, call: &ToolCall) -> ToolOutput {
        let summaries = match self.sources(str_arg(call, "tag_filter")).await {
            Ok(summaries) => summaries,
            Err(e) => return ToolOutput::err(&call.id, format!("list failed: {e}")),
        };
        if summaries.is_empty() {
            return ToolOutput::ok(&call.id, "No memories stored.");
        }
        let limit = usize_arg(call, "limit", 20);
        let mut lines = vec![format!("{} memories stored:", summaries.len())];
        // The store lists oldest first; the model wants the latest.
        for summary in summaries.iter().rev().take(limit) {
            let entity = entity_of(summary)
                .map(|e| format!(" [{e}]"))
                .unwrap_or_default();
            lines.push(format!(
                "- ID={}{entity}: {}",
                summary.source_id,
                framed(&summary.provenance.origin, &summary.label)
            ));
        }
        ToolOutput::ok(&call.id, lines.join("\n"))
    }

    /// The sources of the tool's space, or those with exactly `filter` as a
    /// tag or as their entity. The store matches tags as whole strings.
    async fn sources(&self, filter: Option<&str>) -> Result<Vec<SourceSummary>, MemoryError> {
        let Some(filter) = filter else {
            return self
                .store
                .list_sources(&self.access, &self.space, None)
                .await;
        };
        let mut found = self
            .store
            .list_sources(&self.access, &self.space, Some(filter))
            .await?;
        let by_entity = format!("{ENTITY_TAG_PREFIX}{filter}");
        let entity_matches = self
            .store
            .list_sources(&self.access, &self.space, Some(&by_entity))
            .await?;
        for summary in entity_matches {
            if !found.iter().any(|f| f.source_id == summary.source_id) {
                found.push(summary);
            }
        }
        Ok(found)
    }

    async fn get(&self, call: &ToolCall) -> ToolOutput {
        let Some(id) = str_arg(call, "id") else {
            return ToolOutput::err(&call.id, "get requires 'id'");
        };
        let missing = || ToolOutput::err(&call.id, format!("No memory with ID {id}."));
        let sources = match self.sources(None).await {
            Ok(sources) => sources,
            Err(e) => return ToolOutput::err(&call.id, format!("get failed: {e}")),
        };
        let Some(summary) = sources.into_iter().find(|s| s.source_id.as_str() == id) else {
            return missing();
        };
        let unit_id = UnitId::derive(&self.space, &summary.source_id, &summary.revision, 0);
        match self
            .store
            .get_unit(&self.access, &self.space, &unit_id)
            .await
        {
            Ok(Some(unit)) => {
                let tags: Vec<&str> = summary.tags.iter().map(String::as_str).collect();
                ToolOutput::ok(
                    &call.id,
                    format!(
                        "Memory {id}:\n{}\n\n[tags: {}; origin: {}]",
                        framed(&summary.provenance.origin, &unit.text),
                        tags.join(", "),
                        summary.provenance.origin.label()
                    ),
                )
            }
            Ok(None) | Err(MemoryError::NotFound) => missing(),
            Err(e) => ToolOutput::err(&call.id, format!("get failed: {e}")),
        }
    }
}

fn str_arg<'a>(call: &'a ToolCall, key: &str) -> Option<&'a str> {
    call.args.get(key).and_then(|v| v.as_str())
}

fn usize_arg(call: &ToolCall, key: &str, default: usize) -> usize {
    call.args
        .get(key)
        .and_then(|v| v.as_u64())
        .map_or(default, |n| usize::try_from(n).unwrap_or(usize::MAX))
}

/// What a note is about, from its entity tag.
fn entity_of(summary: &SourceSummary) -> Option<&str> {
    summary
        .tags
        .iter()
        .find_map(|tag| tag.strip_prefix(ENTITY_TAG_PREFIX))
}

/// `text` as it may enter a prompt: quoted unless a person stated it.
fn framed(origin: &Origin, text: &str) -> String {
    if origin.is_untrusted() {
        quote_untrusted(origin.label(), text)
    } else {
        text.to_owned()
    }
}

/// The retrieval as the trajectory records it.
fn to_trajectory(record: RetrievalRecord) -> atif::MemoryRetrieval {
    atif::MemoryRetrieval {
        call_id: String::new(),
        query: record.query,
        space_ids: record.space_ids.iter().map(ToString::to_string).collect(),
        generation_id: record
            .generation_id
            .map_or_else(|| LEXICAL_GENERATION.to_owned(), |g| g.to_string()),
        snapshot_id: record.snapshot_id,
        results: record
            .results
            .into_iter()
            .map(|r| atif::MemoryResult {
                unit_id: r.unit_id.to_string(),
                revision: r.revision.to_string(),
                anchor: atif::MemoryAnchor {
                    heading_path: r.anchor.heading_path,
                    block_ordinal: r.anchor.block_ordinal,
                    text_fingerprint: r.anchor.text_fingerprint,
                },
                score: f64::from(r.score),
                rank: u32::try_from(r.rank).unwrap_or(u32::MAX),
                inserted: r.inserted,
            })
            .collect(),
    }
}

/// Cuts `s` down to at most `max` **bytes**, never inside a character.
///
/// What is being cut is a memory record's content: whatever a user typed or a
/// fetched page contained, so a fixed byte budget routinely lands in the middle
/// of a multi-byte character and `&s[..max]` panics there. Every recall, list
/// and remember goes through here, which makes a single non-ASCII record enough
/// to break recall for every query that matches it.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
    }
}

impl ToolDisplay for SemanticMemoryTool {
    fn display_name(&self) -> &str {
        "SemanticMemory"
    }
    fn icon(&self) -> &str {
        "🧠"
    }
    fn category(&self) -> &str {
        "memory"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        match action {
            "remember" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                format!("remember: {}", truncate(content, 40))
            }
            "recall" => {
                let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("?");
                format!("recall '{q}'")
            }
            _ => action.to_string(),
        }
    }
}
