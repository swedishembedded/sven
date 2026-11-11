// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What the document tools may reach: paths through the session's scope, the
//! backend that parses documents (in this process, or in confined workers
//! when the documents are someone else's), and, in a session of a service
//! that keeps the conversation's files, those files by handle and a way to
//! publish what the tools write as files of the conversation.

use std::sync::Arc;

use sven_document::{DocumentBackend, StandardBackend};
use sven_tool_api::files::{ArtifactRef, FilePublisher, HandleFetcher, HANDLE_PREFIX};
use sven_tool_api::tool::{ToolCall, ToolOutput};
use sven_tool_api::PathScope;

#[derive(Clone)]
pub struct DocumentAccess {
    scope: PathScope,
    backend: Arc<dyn DocumentBackend>,
    fetcher: Option<Arc<dyn HandleFetcher>>,
    publisher: Option<Arc<dyn FilePublisher>>,
}

impl DocumentAccess {
    /// Paths through `scope`, documents parsed in this process.
    #[must_use]
    pub fn local(scope: PathScope) -> Self {
        Self {
            scope,
            backend: Arc::new(StandardBackend),
            fetcher: None,
            publisher: None,
        }
    }

    /// Documents parsed by `backend` instead (confined workers).
    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn DocumentBackend>) -> Self {
        self.backend = backend;
        self
    }

    /// The conversation's files: fetched by handle, and published.
    #[must_use]
    pub fn with_service(
        mut self,
        fetcher: Arc<dyn HandleFetcher>,
        publisher: Arc<dyn FilePublisher>,
    ) -> Self {
        self.fetcher = Some(fetcher);
        self.publisher = Some(publisher);
        self
    }

    pub(crate) fn backend(&self) -> Arc<dyn DocumentBackend> {
        Arc::clone(&self.backend)
    }

    pub(crate) fn scope(&self) -> &PathScope {
        &self.scope
    }

    /// `source` as a path of the scope to read (through [`crate::scoped`]):
    /// itself, or where a handle of the conversation was fetched to.
    pub(crate) async fn source(&self, call: &ToolCall, source: &str) -> Result<String, ToolOutput> {
        let path = if source.starts_with(HANDLE_PREFIX) {
            let Some(fetcher) = &self.fetcher else {
                return Err(ToolOutput::err(
                    &call.id,
                    "this session reaches no service that keeps that file; give a path instead",
                ));
            };
            fetcher
                .fetch(source)
                .await
                .map_err(|e| ToolOutput::err(&call.id, format!("{source}: {e}")))?
        } else {
            source.to_owned()
        };
        self.scope.resolve_for(call, &path)?;
        Ok(path)
    }

    /// `path` (written by a tool) published as a file of the conversation,
    /// when the session has a service to publish to.
    pub(crate) async fn publish(
        &self,
        path: &std::path::Path,
        mime_type: &str,
    ) -> Option<Result<ArtifactRef, String>> {
        let publisher = self.publisher.as_ref()?;
        Some(publisher.publish(path, mime_type).await)
    }
}
