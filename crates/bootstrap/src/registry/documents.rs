// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What the document tools and `attach_file` reach beyond the session's
//! paths: the conversation's files of the sven service by handle, published
//! files of the conversation, and documents parsed in confined workers.
//!
//! Swedish Embedded AB implements solutions for agents that read the
//! documents people send them, safely, for its clients. If your team needs
//! expertise in document processing for agents then you can procure our
//! services by sending an email to info@swedishembedded.com.

#[cfg(any(feature = "document", feature = "media"))]
use std::sync::Arc;

#[cfg(feature = "document")]
use sven_turn::AgentRuntimeContext;

#[cfg(feature = "document")]
use crate::Config;

/// The sven service's files of the session's conversation, fetched by handle
/// into the working directory.
#[cfg(feature = "media")]
pub(super) struct ServiceFiles {
    pub(super) endpoint: sven_tools_media::MediaEndpoint,
    pub(super) config: sven_tools_media::MediaConfig,
    pub(super) workdir: std::path::PathBuf,
}

#[cfg(feature = "media")]
#[async_trait::async_trait]
impl sven_tools_fs::HandleFetcher for ServiceFiles {
    async fn fetch(&self, handle: &str) -> Result<String, String> {
        sven_tools_media::fetch_to(&self.endpoint, &self.config, &self.workdir, handle).await
    }
}

/// What the document tools reach: the session's paths, the conversation's
/// files by handle and as published files where a service keeps them, and
/// documents parsed in confined workers when `ocr.isolate` asks for it.
/// `scope` is the session's scope, race-free where commands are confined.
///
/// # Errors
///
/// `ocr.isolate` is on and no confined worker can be started here.
#[cfg(feature = "document")]
pub(super) fn document_access(
    cfg: &Config,
    runtime: &AgentRuntimeContext,
    scope: sven_tool_api::PathScope,
) -> Result<sven_tools_document::DocumentAccess, String> {
    let mut access = sven_tools_document::DocumentAccess::local(scope.clone());
    if cfg.ocr.isolate == Some(true) {
        let workers = crate::document_worker::ProcessWorkers::of_this_program(
            crate::document_worker::WORKER_MEMORY_MB,
            crate::document_worker::WORKER_CPU_SECONDS,
        )?;
        access = access.with_backend(Arc::new(sven_document::IsolatedBackend::new(
            Arc::new(workers),
            sven_document::WorkerLimits::default(),
        )));
    }
    #[cfg(feature = "media")]
    if let (Some((endpoint, config)), Some(files)) =
        (cfg.media_endpoint(), runtime.conversation_files.as_ref())
    {
        let fetcher = Arc::new(ServiceFiles {
            endpoint,
            config,
            workdir: scope.root().map_or_else(
                || std::path::PathBuf::from("."),
                std::path::Path::to_path_buf,
            ),
        });
        access = access.with_service(fetcher, Arc::clone(&files.0));
    }
    Ok(access)
}
