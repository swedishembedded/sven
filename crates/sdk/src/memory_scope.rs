// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements tenant-scoped agent memory for its clients. If
// your team needs expertise in letting many agents share one memory store
// without sharing what they know then you can procure our services by sending
// an email to info@swedishembedded.com.
//! A scoped memory store for one agent.

use std::sync::Arc;

use sven_memory::SemanticMemoryTool;
use sven_memory_api::{AccessContext, Embedder, MemoryError, MemoryStore, SpaceId};

use crate::agent::Agent;

/// Which store an agent's memory is, which space it works in and what it may do
/// there.
///
/// The host builds the [`AccessContext`]; the store applies it inside its own
/// queries, so an agent cannot read or write outside what the host granted
/// whatever it asks for. The same store can serve many agents, each with its own
/// scope.
pub struct MemoryScope {
    store: Arc<dyn MemoryStore>,
    access: AccessContext,
    space: SpaceId,
    embedder: Option<Arc<dyn Embedder>>,
}

impl MemoryScope {
    /// Scopes memory to `space` of `store` as `access` allows. `access` should
    /// grant ingest and delete in `space` for the agent to remember and forget.
    pub fn new(
        store: Arc<dyn MemoryStore>,
        access: AccessContext,
        space: impl Into<String>,
    ) -> Self {
        Self {
            store,
            access,
            space: SpaceId::new(space),
            embedder: None,
        }
    }

    /// Searches by meaning as well as by word, with vectors from `embedder`.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    pub(crate) async fn tool(self) -> Result<SemanticMemoryTool, MemoryError> {
        let tool = SemanticMemoryTool::new(self.store, self.access, self.space);
        match self.embedder {
            Some(embedder) => tool.with_embedder(embedder).await,
            None => Ok(tool),
        }
    }
}

impl Agent {
    /// Gives this agent `scope` as its memory: the `semantic_memory` tool it
    /// calls reads and writes that space of that store under that access, in
    /// place of any memory the engine or the home directory would give it.
    ///
    /// # Errors
    /// The store's refusal to prepare the embedder's index generation.
    pub async fn with_memory(mut self, scope: MemoryScope) -> Result<Self, MemoryError> {
        self.set_memory_tool(Arc::new(scope.tool().await?));
        Ok(self)
    }
}
