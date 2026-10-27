// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A conversation the server keeps, read in a terminal: this host's replica
//! of its shared document, and what changed in it since the last look.
//!
//! `sven rc show <conversation>` syncs a [`ConversationDoc`] from the server
//! and prints each message once it is finished; `--follow` keeps syncing, so
//! what is written anywhere else - a browser, another terminal - appears
//! here. The replica only reads: what a person adds goes through the server.

use std::collections::HashMap;
use std::time::Duration;

use sven_session_sync::{ConversationDoc, DocMessage};
use uuid::Uuid;

use crate::client::RemoteClient;

/// How often `--follow` asks the server for what changed. A sync with
/// nothing new is a few bytes each way.
pub const FOLLOW_INTERVAL: Duration = Duration::from_secs(1);

/// One thing to show about the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A message is finished (or failed): who wrote it and what it says.
    Message {
        role: String,
        failed: bool,
        text: String,
    },
    /// The agent is writing an answer.
    Answering,
}

impl std::fmt::Display for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message { role, failed, text } => {
                let failed = if *failed { " (failed)" } else { "" };
                write!(f, "{role}{failed}: {text}")
            }
            Self::Answering => write!(f, "(the agent is answering...)"),
        }
    }
}

/// What has been shown of each message: its state when last shown.
#[derive(Debug, Default)]
pub struct Transcript {
    shown: HashMap<String, String>,
}

impl Transcript {
    /// The lines `messages` add to what was shown: each message once it is
    /// finished, and a note when one starts being written. A message is
    /// shown again only if it changes state again.
    pub fn changes(&mut self, messages: &[DocMessage]) -> Vec<Line> {
        let mut lines = Vec::new();
        for message in messages {
            if self.shown.get(&message.id) == Some(&message.state) {
                continue;
            }
            self.shown.insert(message.id.clone(), message.state.clone());
            lines.push(if message.state == "streaming" {
                Line::Answering
            } else {
                Line::Message {
                    role: message.role.clone(),
                    failed: message.state == "failed",
                    text: text_of(&message.parts),
                }
            });
        }
        lines
    }
}

/// A message's parts as one line of text: the text parts as written, and
/// any other part by its type (and name, when it has one), e.g.
/// `[tool_call read_file]`. Parts that are not JSON are shown as they are.
#[must_use]
pub fn text_of(parts: &str) -> String {
    let Ok(serde_json::Value::Array(parts)) = serde_json::from_str::<serde_json::Value>(parts)
    else {
        return parts.to_string();
    };
    parts
        .iter()
        .filter_map(|part| {
            let kind = part.get("type")?.as_str()?;
            if kind == "text" {
                return part.get("text")?.as_str().map(str::to_string);
            }
            let name = part
                .get("name")
                .or_else(|| part.get("message"))
                .and_then(serde_json::Value::as_str);
            Some(match name {
                Some(name) => format!("[{kind} {name}]"),
                None => format!("[{kind}]"),
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One conversation being read: this host's replica of its document, and
/// what of it has been shown.
pub struct Reader {
    conversation: Uuid,
    replica: ConversationDoc,
    transcript: Transcript,
}

impl Reader {
    #[must_use]
    pub fn new(conversation: Uuid) -> Self {
        Self {
            conversation,
            replica: ConversationDoc::new(),
            transcript: Transcript::default(),
        }
    }

    /// Syncs the replica from the server and returns what is new to show.
    ///
    /// # Errors
    ///
    /// The server refuses or cannot be asked, or answers what is not an
    /// update.
    pub async fn read(&mut self, client: &RemoteClient) -> anyhow::Result<Vec<Line>> {
        let update = client
            .sync_conversation(self.conversation, &self.replica.state_vector())
            .await?;
        self.replica.apply(&update)?;
        Ok(self.transcript.changes(&self.replica.messages()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: &str, state: &str, parts: serde_json::Value) -> DocMessage {
        DocMessage {
            id: id.into(),
            role: role.into(),
            author: "a".into(),
            state: state.into(),
            parts: parts.to_string(),
        }
    }

    #[test]
    fn a_message_is_shown_once_when_it_is_finished() {
        let mut transcript = Transcript::default();
        let ask = message(
            "m1",
            "user",
            "complete",
            serde_json::json!([{"type": "text", "text": "list files"}]),
        );
        let writing = message("m2", "assistant", "streaming", serde_json::json!([]));
        assert_eq!(
            transcript.changes(&[ask.clone(), writing]),
            vec![
                Line::Message {
                    role: "user".into(),
                    failed: false,
                    text: "list files".into()
                },
                Line::Answering,
            ]
        );
        let done = message(
            "m2",
            "assistant",
            "complete",
            serde_json::json!([
                {"type": "tool_call", "name": "list_dir", "arguments": {}},
                {"type": "text", "text": "README.md"}
            ]),
        );
        assert_eq!(
            transcript.changes(&[ask.clone(), done.clone()]),
            vec![Line::Message {
                role: "assistant".into(),
                failed: false,
                text: "[tool_call list_dir] README.md".into()
            }]
        );
        assert!(
            transcript.changes(&[ask, done]).is_empty(),
            "nothing changed, nothing shown"
        );
    }

    #[test]
    fn a_failed_answer_says_so_and_unreadable_parts_are_shown_as_they_are() {
        let mut transcript = Transcript::default();
        let failed = message(
            "m1",
            "assistant",
            "failed",
            serde_json::json!([{"type": "error", "message": "the machine went offline"}]),
        );
        assert_eq!(
            transcript.changes(&[failed])[0].to_string(),
            "assistant (failed): [error the machine went offline]"
        );
        assert_eq!(text_of("not json"), "not json");
    }
}

#[cfg(test)]
mod over_http {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use sven_session_sync::{ConversationDoc, DocMessage};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use uuid::Uuid;

    use super::{Line, Reader};
    use crate::client::{ConversationNotFound, RemoteClient};

    /// A server that keeps `server` as conversation `id`'s document for the
    /// token `k`, answering each sync with what the asker's state vector
    /// lacks, and 404 for anything else. Serves `requests` requests.
    async fn serve(id: Uuid, server: std::sync::Arc<ConversationDoc>, requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..requests {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 8192];
                let read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_string();
                let target = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let authorized = request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer k\r\n");
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let response = if authorized
                    && path == format!("/api/workspace/conversations/{id}/doc")
                {
                    let sv = query.strip_prefix("sv=").unwrap_or_default();
                    let body = server.diff(&URL_SAFE_NO_PAD.decode(sv).unwrap()).unwrap();
                    let mut head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    head.extend(body);
                    head
                } else {
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_vec()
                };
                socket.write_all(&response).await.unwrap();
            }
        });
        format!("http://{address}/api/workspace")
    }

    fn message(id: &str, role: &str, state: &str, text: &str) -> DocMessage {
        DocMessage {
            id: id.into(),
            role: role.into(),
            author: "a".into(),
            state: state.into(),
            parts: serde_json::json!([{"type": "text", "text": text}]).to_string(),
        }
    }

    #[tokio::test]
    async fn a_reader_follows_the_servers_document_and_shows_each_change_once() {
        let id = Uuid::new_v4();
        let server = std::sync::Arc::new(ConversationDoc::with_client_id(1));
        server.add_message(&message("m1", "user", "complete", "list files"));
        server.add_message(&message("m2", "assistant", "streaming", ""));
        let control = serve(id, std::sync::Arc::clone(&server), 3).await;
        let client = RemoteClient::new(&control, "k").unwrap();
        let mut reader = Reader::new(id);

        assert_eq!(
            reader.read(&client).await.unwrap(),
            vec![
                Line::Message {
                    role: "user".into(),
                    failed: false,
                    text: "list files".into()
                },
                Line::Answering,
            ]
        );
        assert!(reader.read(&client).await.unwrap().is_empty());
        server.update_message(
            "m2",
            "complete",
            &serde_json::json!([{"type": "text", "text": "README.md"}]).to_string(),
        );
        assert_eq!(
            reader.read(&client).await.unwrap(),
            vec![Line::Message {
                role: "assistant".into(),
                failed: false,
                text: "README.md".into()
            }]
        );
    }

    #[tokio::test]
    async fn a_conversation_that_does_not_run_here_is_not_found() {
        let id = Uuid::new_v4();
        let control = serve(id, std::sync::Arc::new(ConversationDoc::new()), 1).await;
        let client = RemoteClient::new(&control, "k").unwrap();
        let error = Reader::new(Uuid::new_v4()).read(&client).await.unwrap_err();
        assert!(
            error.downcast_ref::<ConversationNotFound>().is_some(),
            "{error}"
        );
    }
}
