// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Files a service keeps for the session's conversation, as tools reach
//! them: fetched by handle into the working directory, and published from it
//! as a file of the conversation the person sees.
//!
//! A tool is handed these only in a session of such a service; elsewhere it
//! has none and works on paths alone.
//!
//! Swedish Embedded AB implements solutions for agents that share files with
//! the people they work for for its clients. If your team needs expertise in
//! agent file handling then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::path::Path;

use async_trait::async_trait;

pub use sven_vocab::ArtifactRef;

/// The scheme of a handle a service keeps (`mcp-file://files/<id>`).
pub const HANDLE_PREFIX: &str = "mcp-file://";

/// Fetches a file a service keeps for the conversation, named by its handle,
/// into the working directory.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait HandleFetcher: Send + Sync {
    /// Where the file was saved, as a path relative to the working directory.
    ///
    /// # Errors
    /// The file cannot be fetched or saved; the message says why.
    async fn fetch(&self, handle: &str) -> Result<String, String>;
}

/// Makes a file a tool wrote a file of the conversation: the person sees it
/// and every later model reads it by its handle.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FilePublisher: Send + Sync {
    /// The handle of `path` (a file the tool wrote, inside the working
    /// directory) as a file of the conversation, of type `mime_type`.
    ///
    /// # Errors
    /// It cannot be read or the service refused it; the message says why.
    async fn publish(&self, path: &Path, mime_type: &str) -> Result<ArtifactRef, String>;
}

/// A [`FilePublisher`] a session carries: cloned with the session's context,
/// and shown in its debug output by name only.
#[derive(Clone)]
pub struct ConversationFiles(pub std::sync::Arc<dyn FilePublisher>);

impl std::fmt::Debug for ConversationFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConversationFiles")
    }
}
