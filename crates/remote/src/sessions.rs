// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The sessions this host ran, kept so a restart of `sven rc` continues them
//! as they were.
//!
//! The server hands a session it asks this host to open the conversation so
//! far, but only as flat text: tool calls and their results are gone. The
//! host keeps each session's own conversation - every message, tool calls
//! included - after every finished turn, in
//! `<data dir>/sven/remote/sessions/<session id>.json`, owner-only (it holds
//! the conversation). On opening a session it has kept, it continues from
//! that, not from the flat text.

use std::path::{Path, PathBuf};

use sven_model::Message;
use uuid::Uuid;

/// Where this user's kept sessions live.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("sven").join("remote").join("sessions"))
}

/// The kept conversations of one host, by session id.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn file(&self, session: Uuid) -> PathBuf {
        self.dir.join(format!("{session}.json"))
    }

    /// The conversation kept for `session`, if any. A file that cannot be
    /// read is reported and treated as absent: the server's history is the
    /// fallback, never a guess.
    #[must_use]
    pub fn load(&self, session: Uuid) -> Option<Vec<Message>> {
        let path = self.file(session);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!("{}: {e}", path.display());
                return None;
            }
        };
        match serde_json::from_str(&text) {
            Ok(messages) => Some(messages),
            Err(e) => {
                tracing::warn!(
                    "{} is not a kept session ({e}); continuing from the server's history",
                    path.display()
                );
                None
            }
        }
    }

    /// Keeps `messages` as `session`'s conversation, replacing what was kept.
    ///
    /// # Errors
    ///
    /// The directory cannot be made or the file written.
    pub fn save(&self, session: Uuid, messages: &[Message]) -> anyhow::Result<()> {
        sven_model_drivers::credentials::write_private(
            &self.file(session),
            &serde_json::to_string(messages)?,
        )
    }

    /// The directory, for messages.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// What a session opens with: its own kept conversation when the host has
/// one, else what the server handed over.
#[must_use]
pub fn opening_history(kept: Option<Vec<Message>>, from_server: Vec<Message>) -> Vec<Message> {
    kept.unwrap_or(from_server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("sven-remote-sessions-{}", Uuid::new_v4()))
    }

    #[test]
    fn a_kept_conversation_comes_back_whole_tool_calls_included_and_owner_only() {
        let dir = scratch();
        let store = SessionStore::new(&dir);
        let session = Uuid::new_v4();
        assert!(store.load(session).is_none());

        let messages = vec![
            Message::user("list files"),
            Message::tool_call("call_1", "list_dir", r#"{"path":"."}"#),
            Message::assistant("There is one file."),
        ];
        store.save(session, &messages).unwrap();
        let back = store.load(session).expect("kept");
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&messages).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.file(session))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "a kept conversation is its owner's alone");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_opens_from_its_own_record_before_the_servers_flat_text() {
        let kept = vec![Message::user("a"), Message::assistant("b")];
        let flat = vec![Message::user("only text")];
        assert_eq!(opening_history(Some(kept.clone()), flat.clone()).len(), 2);
        assert_eq!(opening_history(None, flat).len(), 1);
    }

    #[test]
    fn an_unreadable_record_falls_back_to_the_server_rather_than_guessing() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let store = SessionStore::new(&dir);
        let session = Uuid::new_v4();
        std::fs::write(store.file(session), "not json").unwrap();
        assert!(store.load(session).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
