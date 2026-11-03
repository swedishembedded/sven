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
//!    [`RemoteDiscovery`]: where a person types the code that approves a new
//!    host (`authorize_url`), the two endpoints of the device authorization
//!    grant (`device_authorization_url`, `device_token_url`), where the host
//!    talks to the server (`control_url`), and the OpenAI-compatible endpoint
//!    its sessions call models through.
//!    **Signing in** with `authorization_endpoint` and `token_endpoint` is the
//!    authorization code grant with PKCE (RFC 7636) over a loopback redirect
//!    (RFC 8252): the host opens `authorization_endpoint?response_type=code&
//!    client_id=sven&redirect_uri&state&code_challenge&code_challenge_method=
//!    S256&name`, the browser returns to `http://127.0.0.1:<port>/callback`
//!    with `code` and `state`, and the host `POST`s `{"grant_type":
//!    "authorization_code","code","code_verifier","redirect_uri","client_id"}`
//!    to `token_endpoint`, which answers `{"token","account"}` once or `400`
//!    `{"error","error_description"?}`. Without a reachable loopback port the
//!    redirect is `code_display_uri`, which shows the code to paste into the
//!    terminal. A server that offers neither endpoint is signed in to with
//!    the older device grant, **RFC 8628**: the host `POST`s `{"name"}` to
//!    `device_authorization_url` and gets `{device_code, user_code,
//!    verification_uri, expires_in, interval}`; the person types `user_code`
//!    at `verification_uri` and approves; meanwhile the host `POST`s
//!    `{"device_code"}` to `device_token_url` every `interval` seconds. A
//!    `400` answers `authorization_pending` (go on), `slow_down` (add 5 s to
//!    the interval), `access_denied` (optional `error_description`) or
//!    `expired_token` (also an unknown or redeemed code); `200` answers
//!    `{"token", "account"}` exactly once. The request endpoint answers `429`
//!    with `Retry-After` when called too often. The device code never leaves
//!    the host's process; the token never appears in a browser.
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
    /// The page where a person types the code a host shows, to let that host
    /// act for them (the grant's `verification_uri`).
    pub authorize_url: String,
    /// Where a host asks for a device code. Absent on a server that predates
    /// the device grant, which `sven auth login` then refuses.
    #[serde(default)]
    pub device_authorization_url: Option<String>,
    /// Where a host polls for the token once the person approved.
    #[serde(default)]
    pub device_token_url: Option<String>,
    /// Where a person approves a host in the browser: the authorization
    /// endpoint of the authorization code grant with PKCE (RFC 7636). Present
    /// together with [`Self::token_endpoint`] on a server that offers it;
    /// `sven auth login` then prefers it to the device grant.
    #[serde(default)]
    pub authorization_endpoint: Option<String>,
    /// Where a host redeems the one-time code, with its PKCE verifier, for
    /// the token.
    #[serde(default)]
    pub token_endpoint: Option<String>,
    /// The page that shows the one-time code for the person to paste into a
    /// terminal, used as the redirect when the browser cannot reach the
    /// host's loopback port (a remote or headless machine).
    #[serde(default)]
    pub code_display_uri: Option<String>,
    /// The base of `whoami`, `commands` and `events`.
    pub control_url: String,
    /// The OpenAI-compatible API (ending in `/v1`) the host's sessions call
    /// models through, with the same token.
    pub openai_base_url: String,
    /// The MCP endpoint of the person's connected accounts, reached with the
    /// same token. Absent on a server that offers none.
    #[serde(default)]
    pub mcp_url: Option<String>,
    /// Where the host trades its token for a short-lived one of the MCP
    /// endpoint. Absent on a server whose MCP endpoint takes the host's own.
    #[serde(default)]
    pub mcp_token_url: Option<String>,
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
    use crate::{HistoryEntry, HistoryRole, SlashCommandInfo};
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
    fn slash_commands_are_asked_for_and_listed_in_the_shape_a_server_uses() {
        let asked: CommandEnvelope =
            serde_json::from_str(r#"{"seq":3,"command":{"type":"list_slash_commands"}}"#).unwrap();
        assert!(matches!(asked.command, ControlCommand::ListSlashCommands));

        let listed = ControlEvent::SlashCommands {
            commands: vec![SlashCommandInfo {
                name: "review-pr".into(),
                description: "Review a pull request".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(&listed).unwrap(),
            serde_json::json!({
                "type": "slash_commands",
                "commands": [{"name": "review-pr", "description": "Review a pull request"}],
            })
        );
    }

    #[test]
    fn a_tool_call_is_described_in_the_shape_a_server_reads() {
        let session = Uuid::parse_str(SESSION).unwrap();
        let started = ControlEvent::ToolCallDescribed {
            session_id: session,
            call_id: "c1".into(),
            action: "Shell".into(),
            title: "ls -la".into(),
            category: "shell".into(),
            duration_ms: None,
        };
        assert_eq!(
            serde_json::to_value(&started).unwrap(),
            serde_json::json!({"type": "tool_call_described", "session_id": SESSION,
                "call_id": "c1", "action": "Shell", "title": "ls -la", "category": "shell"})
        );
        let finished = ControlEvent::ToolCallDescribed {
            session_id: session,
            call_id: "c1".into(),
            action: "Shell".into(),
            title: "ls -la".into(),
            category: "shell".into(),
            duration_ms: Some(1_250),
        };
        assert_eq!(
            serde_json::to_value(&finished).unwrap()["duration_ms"],
            1_250
        );
    }

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
                            images: Vec::new(),
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
    fn a_finished_call_carries_its_images_on_the_wire_only_when_it_has_any() {
        let finished = |images| SessionEvent::ToolCallFinished {
            call_id: "call_1".into(),
            tool_name: "read_file".into(),
            output: "Image file: a.png".into(),
            is_error: false,
            images,
        };
        assert_eq!(
            serde_json::to_value(finished(vec![sven_vocab::ToolImage {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            }]))
            .unwrap(),
            serde_json::json!({"ToolCallFinished": {"call_id": "call_1", "tool_name": "read_file",
                "output": "Image file: a.png", "is_error": false,
                "images": [{"media_type": "image/png", "data": "iVBORw0KGgo="}]}})
        );
        let plain = serde_json::to_value(finished(Vec::new())).unwrap();
        assert!(plain["ToolCallFinished"].get("images").is_none());
        // A host from before the field sends no `images`.
        let old: SessionEvent = serde_json::from_value(plain).unwrap();
        assert_eq!(old, finished(Vec::new()));
    }

    #[test]
    fn discovery_device_fields_are_optional() {
        let with: RemoteDiscovery = serde_json::from_str(
            r#"{"protocol_version":1,"authorize_url":"https://s/connect/sven",
                "device_authorization_url":"https://s/api/w/device",
                "device_token_url":"https://s/api/w/device/token",
                "control_url":"https://s/api/w","openai_base_url":"https://s/v1"}"#,
        )
        .unwrap();
        assert_eq!(
            with.device_authorization_url.as_deref(),
            Some("https://s/api/w/device")
        );
        assert_eq!(
            with.device_token_url.as_deref(),
            Some("https://s/api/w/device/token")
        );
        let without: RemoteDiscovery = serde_json::from_str(
            r#"{"protocol_version":1,"authorize_url":"https://s/connect/sven",
                "control_url":"https://s/api/w","openai_base_url":"https://s/v1"}"#,
        )
        .unwrap();
        assert!(without.device_authorization_url.is_none() && without.device_token_url.is_none());
    }

    #[test]
    fn discovery_code_grant_fields_are_optional() {
        let with: RemoteDiscovery = serde_json::from_str(
            r#"{"protocol_version":1,"authorize_url":"https://s/connect/sven",
                "authorization_endpoint":"https://s/connect/sven/authorize",
                "token_endpoint":"https://s/api/workspace/token",
                "code_display_uri":"https://s/connect/sven/code",
                "control_url":"https://s/api/w","openai_base_url":"https://s/v1"}"#,
        )
        .unwrap();
        assert_eq!(
            with.authorization_endpoint.as_deref(),
            Some("https://s/connect/sven/authorize")
        );
        assert_eq!(
            with.token_endpoint.as_deref(),
            Some("https://s/api/workspace/token")
        );
        assert_eq!(
            with.code_display_uri.as_deref(),
            Some("https://s/connect/sven/code")
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
