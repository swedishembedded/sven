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
//!
//! It is also kept after every finished tool call, so a turn the host was
//! stopped in the middle of is not lost: [`reopen`] keeps the work it did,
//! reports a call that was cut off as interrupted (it may or may not have
//! taken effect, so it is never run again on its own), and names the
//! request the turn was answering, so the server asking again continues
//! that turn instead of starting it over.

use std::path::{Path, PathBuf};

use sven_model::{Message, MessageContent, Role};
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
/// one ([`reopen`]ed), else what the server handed over.
#[must_use]
pub fn opening(kept: Option<Vec<Message>>, from_server: Vec<Message>) -> Reopened {
    kept.map_or(
        Reopened {
            history: from_server,
            unfinished: None,
        },
        reopen,
    )
}

/// What a tool call cut off by the host stopping answers.
pub const INTERRUPTED_CALL: &str = "interrupted: the agent's host stopped before this call \
finished; it may or may not have taken effect. Check before doing it again.";

/// What the host sends instead of a request it was already answering when
/// it stopped: the work above is that answer's, so far.
pub const CONTINUE_INTERRUPTED: &str = "(The answer to my last message was interrupted when \
the agent's host restarted. The tool calls and results above are from that attempt; a call \
marked interrupted may or may not have taken effect. Continue from where it stopped.)";

/// A kept conversation, ready to continue.
#[derive(Debug)]
pub struct Reopened {
    pub history: Vec<Message>,
    /// The request whose answer the host stopped in the middle of, if any.
    pub unfinished: Option<String>,
}

/// Prepares a kept conversation to continue: a turn that ended with the
/// model's answer is as it was; one that did not keeps its tool calls and
/// results, each call left without a result gets [`INTERRUPTED_CALL`], and
/// the request it was answering is named.
#[must_use]
pub fn reopen(mut history: Vec<Message>) -> Reopened {
    let Some(asked) = history.iter().rposition(|m| m.role == Role::User) else {
        return Reopened {
            history,
            unfinished: None,
        };
    };
    let answered = matches!(
        history.last(),
        Some(Message { role: Role::Assistant, content, .. })
            if !matches!(content, MessageContent::ToolCall { .. })
    );
    if answered {
        return Reopened {
            history,
            unfinished: None,
        };
    }
    let unfinished = history[asked].as_text().map(str::to_string);
    let results: std::collections::HashSet<String> = history[asked..]
        .iter()
        .filter_map(|m| match &m.content {
            MessageContent::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let cut_off: Vec<String> = history[asked..]
        .iter()
        .filter_map(|m| match &m.content {
            MessageContent::ToolCall { tool_call_id, .. } if !results.contains(tool_call_id) => {
                Some(tool_call_id.clone())
            }
            _ => None,
        })
        .collect();
    history.extend(
        cut_off
            .into_iter()
            .map(|id| Message::tool_result(id, INTERRUPTED_CALL)),
    );
    Reopened {
        history,
        unfinished,
    }
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
        assert_eq!(opening(Some(kept.clone()), flat.clone()).history.len(), 2);
        assert_eq!(opening(None, flat).history.len(), 1);
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

    #[test]
    fn a_finished_turn_reopens_as_it_was() {
        let kept = vec![Message::user("hi"), Message::assistant("hello")];
        let reopened = reopen(kept);
        assert_eq!(reopened.history.len(), 2);
        assert_eq!(reopened.unfinished, None);
    }

    #[test]
    fn a_turn_stopped_mid_run_keeps_its_work_and_never_reruns_a_cut_off_call() {
        let kept = vec![
            Message::user("hi"),
            Message::assistant("hello"),
            Message::user("deploy it"),
            Message::tool_call("c1", "shell", r#"{"cmd":"make build"}"#),
            Message::tool_result("c1", "built"),
            Message::tool_call("c2", "shell", r#"{"cmd":"make deploy"}"#),
        ];
        let reopened = reopen(kept);
        assert_eq!(reopened.unfinished.as_deref(), Some("deploy it"));
        assert_eq!(
            reopened.history.len(),
            7,
            "the work is kept, one result added"
        );
        match &reopened.history[6].content {
            MessageContent::ToolResult { tool_call_id, .. } => assert_eq!(tool_call_id, "c2"),
            other => panic!("expected the cut-off call's result, got {other:?}"),
        }
        let json = serde_json::to_string(&reopened.history[6]).unwrap();
        assert!(json.contains("interrupted"), "{json}");
        assert!(
            serde_json::to_string(&reopened.history[4])
                .unwrap()
                .contains("built"),
            "a finished call keeps its own result"
        );
    }
}
