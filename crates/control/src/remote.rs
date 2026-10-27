// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for operating coding agents from
// anywhere -- a browser, a phone, a second terminal -- while they work on the
// machine that has the code, for its clients. If your team needs expertise in
// remote agent control then you can procure our services by sending an email
// to info@swedishembedded.com.
//! The HTTP binding of the control protocol, for a **remote-control server**.
//!
//! A host (`sven rc`) runs agent sessions on the user's machine and is
//! controlled through a server it connects OUT to, so the machine never
//! accepts a connection. Everything is plain HTTPS with a bearer token:
//!
//! 1. **Discovery.** `GET {server}/.well-known/sven-remote` answers a
//!    [`RemoteDiscovery`]: where a person approves a new host
//!    (`authorize_url`), where the host talks to the server (`control_url`),
//!    and the OpenAI-compatible endpoint its sessions call models through.
//! 2. **Identity.** `GET {control_url}/whoami` with the token answers a
//!    [`WhoAmI`]; `401` means the token is not (or no longer) valid.
//! 3. **Environment.** `POST {control_url}/environment` with
//!    `{"path", "run"}` attaches the host to the environment of the directory
//!    it works in (`path`, canonical; `hosted` for a server's hosted host)
//!    and answers `{"environment_id"}`. Starting again in the same directory
//!    attaches to the same environment, so its conversations are connected
//!    again; `409` while another live host (another `run`) holds it. Every
//!    command and event request below names both as `?environment=ID&run=RUN`
//!    and is refused with `409` once the run is no longer the environment's
//!    (another host took over, or this one let it go): that host stops. A
//!    stopping host lets it go:
//!    `DELETE {control_url}/environment?environment=ID&run=RUN`.
//!    ([`AttachRequest`], [`Attached`])
//! 4. **Commands.** `GET {control_url}/commands?after=N` is a server-sent
//!    event stream. Each `data:` line is one [`CommandEnvelope`] with a
//!    `seq` above `N`, in order; comment lines are keep-alives. A command
//!    the host has not acknowledged is sent again on the next connection, so
//!    delivery is at least once and the host drops a `seq` it already took.
//! 5. **Events.** `POST {control_url}/events` with an [`EventBatch`]: the
//!    host's events in order, numbered from 1 within the host's `run` (a
//!    fresh id each time the host starts), plus the highest command `seq` it
//!    has taken. The server drops an event whose `(run, seq)` it already
//!    stored, so a batch sent twice is stored once and a restarted host is
//!    never mistaken for one replaying old events. Numbers stay small enough
//!    to be exact in any JSON reader. A host uploads at least every 15 s,
//!    even with nothing to say, and at once when it takes a command: that is
//!    how the server knows it is alive (a host silent for 45 s counts as
//!    gone), and `working` names the sessions mid-turn, whose answers are
//!    still coming.
//! 6. **Conversations.** `GET {control_url}/conversations/{id}/doc?sv=S`
//!    answers, as bytes, the update (`sven-session-sync`'s v1 encoding) that
//!    brings a replica whose state vector is `S` (base64url, empty for an
//!    empty replica) up to the server's copy of the conversation's shared
//!    document. Only a conversation that runs on this host is found
//!    ([`conversation_doc_endpoint`]).
//! 7. **Attached terminals.** `POST {control_url}/conversations/{id}/messages`
//!    with `{"text": ...}` adds a message to a conversation that runs on
//!    this host, exactly as one sent from the server's own interface: the
//!    server stores it, then asks this host to answer it. `409` while an
//!    answer is being written ([`conversation_messages_endpoint`]).
//!    `POST {control_url}/conversations/{id}/approvals` with
//!    `{"callId": ..., "approve": bool}` allows or refuses a tool call that
//!    waits there, as the server's own interface does; `404` when nothing
//!    waits for it ([`conversation_approvals_endpoint`]).
//!
//! The server is trusted with the conversation and nothing else: it can ask
//! a session to do work, and the session's own permission policy still
//! decides what runs on the machine.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ControlCommand, ControlEvent};

/// The protocol revision this build speaks. A server that answers a higher
/// one is refused rather than half-understood.
pub const REMOTE_PROTOCOL_VERSION: u32 = 1;

/// Where a server publishes its [`RemoteDiscovery`], relative to its origin.
pub const DISCOVERY_PATH: &str = "/.well-known/sven-remote";

/// What a server says about itself. Unknown fields are ignored, so a server
/// may add some without breaking an older host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteDiscovery {
    pub protocol_version: u32,
    /// The page a person opens to let a host act for them. The host appends
    /// `?name=<host name>`.
    pub authorize_url: String,
    /// The base of `whoami`, `commands` and `events`.
    pub control_url: String,
    /// The OpenAI-compatible API (ending in `/v1`) the host's sessions call
    /// models through, with the same token.
    pub openai_base_url: String,
}

/// Who a token belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoAmI {
    /// The server's id for this host.
    pub workspace_id: Uuid,
    /// The name the host registered with.
    pub workspace_name: String,
    /// The account the host acts for, as the server shows it to a person.
    pub account: String,
}

/// One command on the command stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEnvelope {
    /// Strictly increasing per host; the acknowledgement cursor.
    pub seq: u64,
    pub command: ControlCommand,
}

/// One event the host uploads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Strictly increasing within the batch's `run`, from 1. With `run`, the
    /// deduplication key.
    pub seq: u64,
    pub event: ControlEvent,
}

/// One upload: events in the order they happened, and how far the host has
/// taken the command stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBatch {
    /// This run of the host: a fresh id each time it starts.
    pub run: Uuid,
    #[serde(default)]
    pub events: Vec<EventEnvelope>,
    /// The highest command `seq` the host has taken, when it took any since
    /// the last upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_through: Option<u64>,
    /// The sessions mid-turn as this batch is sent: the server keeps their
    /// answers alive, and ends any other answer it waits on from this host
    /// once it has been silent too long.
    #[serde(default)]
    pub working: Vec<Uuid>,
}

/// `control_url` joined with one of its endpoints, without a doubled slash.
#[must_use]
pub fn control_endpoint(control_url: &str, endpoint: &str) -> String {
    format!(
        "{}/{}",
        control_url.trim_end_matches('/'),
        endpoint.trim_start_matches('/')
    )
}

/// What a host sends to attach to its environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachRequest {
    /// The directory the host works in, canonical; `hosted` for a server's
    /// hosted host.
    pub path: String,
    /// This run of the host.
    pub run: Uuid,
}

/// The environment a host attached to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attached {
    pub environment_id: Uuid,
}

/// The path a hosted host attaches with.
pub const HOSTED_ENVIRONMENT: &str = "hosted";

/// The endpoint, relative to `control_url`, that decides a tool call
/// waiting in conversation `id`.
#[must_use]
pub fn conversation_approvals_endpoint(id: Uuid) -> String {
    format!("conversations/{id}/approvals")
}

/// The endpoint, relative to `control_url`, that adds a message to
/// conversation `id`.
#[must_use]
pub fn conversation_messages_endpoint(id: Uuid) -> String {
    format!("conversations/{id}/messages")
}

/// The endpoint, relative to `control_url`, of conversation `id`'s shared
/// document.
#[must_use]
pub fn conversation_doc_endpoint(id: Uuid) -> String {
    format!("conversations/{id}/doc")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HistoryEntry, HistoryRole};
    use sven_vocab::{AgentMode, SessionEvent, ToolCall};

    const SESSION: &str = "6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10";

    /// The exact JSON a server sends on the command stream. A server written
    /// in another language is held to these literals, so they are spelled out
    /// rather than produced by serializing.
    #[test]
    fn a_server_command_stream_parses() {
        let open: CommandEnvelope = serde_json::from_str(&format!(
            r#"{{"seq":7,"command":{{"type":"new_session","id":"{SESSION}","mode":"agent",
                "working_dir":null,"history":[{{"role":"user","text":"hi"}},{{"role":"assistant","text":"hello"}}]}}}}"#
        ))
        .unwrap();
        assert_eq!(open.seq, 7);
        match open.command {
            ControlCommand::NewSession { mode, history, .. } => {
                assert_eq!(mode, AgentMode::Agent);
                assert_eq!(
                    history,
                    vec![
                        HistoryEntry {
                            role: HistoryRole::User,
                            text: "hi".into()
                        },
                        HistoryEntry {
                            role: HistoryRole::Assistant,
                            text: "hello".into()
                        },
                    ]
                );
            }
            other => panic!("expected NewSession, got {other:?}"),
        }
        // A session opened before `history` existed still parses.
        let bare: CommandEnvelope = serde_json::from_str(&format!(
            r#"{{"seq":8,"command":{{"type":"new_session","id":"{SESSION}","mode":"agent","working_dir":null}}}}"#
        ))
        .unwrap();
        assert!(
            matches!(bare.command, ControlCommand::NewSession { history, .. } if history.is_empty())
        );

        for (json, check) in [
            (
                format!(
                    r#"{{"seq":9,"command":{{"type":"set_model","session_id":"{SESSION}","model":"deepseek/flash"}}}}"#
                ),
                "set_model",
            ),
            (
                format!(
                    r#"{{"seq":10,"command":{{"type":"send_input","session_id":"{SESSION}","text":"list files"}}}}"#
                ),
                "send_input",
            ),
            (
                format!(
                    r#"{{"seq":11,"command":{{"type":"cancel_session","session_id":"{SESSION}"}}}}"#
                ),
                "cancel_session",
            ),
        ] {
            let parsed: CommandEnvelope = serde_json::from_str(&json).unwrap();
            let kind = serde_json::to_value(&parsed.command).unwrap()["type"].clone();
            assert_eq!(kind, check);
        }
    }

    /// The exact JSON a host uploads, as a server will parse it.
    #[test]
    fn an_event_batch_has_the_shape_a_server_reads() {
        let session = Uuid::parse_str(SESSION).unwrap();
        let run = Uuid::parse_str("0b5f3c2e-1d4a-4e8b-9f10-2a3b4c5d6e7f").unwrap();
        let batch = EventBatch {
            run,
            events: vec![
                EventEnvelope {
                    seq: 1,
                    event: ControlEvent::Session {
                        session_id: session,
                        event: SessionEvent::TextDelta("Hel".into()),
                    },
                },
                EventEnvelope {
                    seq: 2,
                    event: ControlEvent::Session {
                        session_id: session,
                        event: SessionEvent::ToolCallStarted(ToolCall {
                            id: "call_1".into(),
                            name: "list_dir".into(),
                            args: serde_json::json!({"path": "."}),
                        }),
                    },
                },
                EventEnvelope {
                    seq: 3,
                    event: ControlEvent::Session {
                        session_id: session,
                        event: SessionEvent::ToolCallFinished {
                            call_id: "call_1".into(),
                            tool_name: "list_dir".into(),
                            output: "Cargo.toml".into(),
                            is_error: false,
                        },
                    },
                },
                EventEnvelope {
                    seq: 4,
                    event: ControlEvent::Session {
                        session_id: session,
                        event: SessionEvent::TurnComplete,
                    },
                },
                EventEnvelope {
                    seq: 5,
                    event: ControlEvent::SessionState {
                        session_id: session,
                        state: crate::SessionState::Idle,
                    },
                },
            ],
            acked_through: Some(11),
            working: vec![session],
        };
        assert_eq!(
            serde_json::to_value(&batch).unwrap(),
            serde_json::json!({
                "run": "0b5f3c2e-1d4a-4e8b-9f10-2a3b4c5d6e7f",
                "events": [
                    {"seq": 1, "event": {"type": "session", "session_id": SESSION, "event": {"TextDelta": "Hel"}}},
                    {"seq": 2, "event": {"type": "session", "session_id": SESSION,
                        "event": {"ToolCallStarted": {"id": "call_1", "name": "list_dir", "args": {"path": "."}}}}},
                    {"seq": 3, "event": {"type": "session", "session_id": SESSION,
                        "event": {"ToolCallFinished": {"call_id": "call_1", "tool_name": "list_dir",
                            "output": "Cargo.toml", "is_error": false}}}},
                    {"seq": 4, "event": {"type": "session", "session_id": SESSION, "event": "TurnComplete"}},
                    {"seq": 5, "event": {"type": "session_state", "session_id": SESSION, "state": "idle"}}
                ],
                "acked_through": 11,
                "working": [SESSION]
            })
        );
    }

    #[test]
    fn discovery_ignores_fields_it_does_not_know() {
        let found: RemoteDiscovery = serde_json::from_str(
            r#"{"protocol_version":1,"authorize_url":"https://s/connect/sven","control_url":"https://s/api/sven",
                "openai_base_url":"https://s/v1","added_later":true}"#,
        )
        .unwrap();
        assert_eq!(found.protocol_version, REMOTE_PROTOCOL_VERSION);
        assert_eq!(
            control_endpoint(&found.control_url, "commands"),
            "https://s/api/sven/commands"
        );
        assert_eq!(
            control_endpoint("https://s/api/sven/", "/events"),
            "https://s/api/sven/events"
        );
    }
}
