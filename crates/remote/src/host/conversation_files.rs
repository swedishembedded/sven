// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that hand their work
// back to the people they work for for its clients. If your team needs
// expertise in remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! A file a session's tool wrote, uploaded as a file of the session's own
//! conversation (`document_convert`'s Markdown): the person sees it there,
//! and a model reads it by its handle. Only a file inside the session's
//! working directory is uploaded, and only to that conversation.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use sven_tool_api::files::{ArtifactRef, FilePublisher, HANDLE_PREFIX};
use uuid::Uuid;

use crate::client::RemoteClient;

pub(super) struct ConversationUpload {
    pub(super) client: RemoteClient,
    pub(super) conversation: Uuid,
    pub(super) working_dir: PathBuf,
}

impl ConversationUpload {
    /// `path` when it is a file inside the working directory, resolved.
    fn inside(&self, path: &Path) -> Result<PathBuf, String> {
        let root = self
            .working_dir
            .canonicalize()
            .map_err(|e| format!("the working directory: {e}"))?;
        let file = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !file.starts_with(&root) || !file.is_file() {
            return Err(format!(
                "{} is not a file of this conversation's directory",
                path.display()
            ));
        }
        Ok(file)
    }
}

#[async_trait]
impl FilePublisher for ConversationUpload {
    async fn publish(&self, path: &Path, mime_type: &str) -> Result<ArtifactRef, String> {
        let file = self.inside(path)?;
        let size = std::fs::metadata(&file).map_err(|e| e.to_string())?.len();
        let id = self
            .client
            .upload_to_conversation(self.conversation, &file)
            .await
            .map_err(|e| format!("{e:#}"))?;
        Ok(ArtifactRef {
            uri: format!("{HANDLE_PREFIX}files/{id}"),
            name: file
                .file_name()
                .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned()),
            mime_type: mime_type.to_owned(),
            size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_file_of_the_working_directory_is_uploaded() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "x").unwrap();
        std::fs::write(outside.path().join("b.md"), "x").unwrap();
        let upload = ConversationUpload {
            client: RemoteClient::new("http://127.0.0.1:9/api", "token").unwrap(),
            conversation: Uuid::nil(),
            working_dir: dir.path().to_path_buf(),
        };
        assert!(upload.inside(&dir.path().join("a.md")).is_ok());
        assert!(upload.inside(&outside.path().join("b.md")).is_err());
        assert!(upload.inside(&dir.path().join("../b.md")).is_err());
        assert!(
            upload.inside(dir.path()).is_err(),
            "a directory is not a file"
        );
    }
}
