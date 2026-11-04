// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The host's supervisor of its sessions: hands each command to its
//! session's actor without ever waiting on one, starts an actor on first
//! sight and again after it died, and stops one that stopped taking its
//! commands, so no conversation can hold up another.

use std::collections::HashMap;
use std::sync::Arc;

use sven_control::{ControlCommand, ControlEvent, SessionState};
use sven_machines::AgentEvent;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::answers::Input;
use super::upload::Upload;
use super::{slash, SessionActor, SessionCommand, SessionContext, SESSION_QUEUE};

/// Owns the sessions: starts each one's actor on first sight and hands it its
/// commands. The host takes no input of its own: a person writes to a
/// conversation from wherever it is open (`sven rc attach`, the server).
pub(super) async fn router(mut rx: mpsc::Receiver<ControlCommand>, context: Arc<SessionContext>) {
    let mut sessions: HashMap<Uuid, Mailbox> = HashMap::new();
    while let Some(command) = rx.recv().await {
        route(command, &mut sessions, &context).await;
    }
}

/// Hands `command` to its session, starting the session's actor when this is
/// the first the host hears of it.
async fn route(
    command: ControlCommand,
    sessions: &mut HashMap<Uuid, Mailbox>,
    context: &Arc<SessionContext>,
) {
    let (session_id, message) = match command {
        ControlCommand::NewSession {
            id,
            mode,
            history,
            replace_history,
            ..
        } => (
            id,
            SessionCommand::Open {
                mode,
                history,
                replace: replace_history,
            },
        ),
        ControlCommand::SetModel { session_id, model } => {
            (session_id, SessionCommand::Model(model))
        }
        ControlCommand::SendInput {
            session_id,
            text,
            answer_id,
            attachments,
        } => (
            session_id,
            SessionCommand::Input(Input {
                text,
                answer_id,
                attachments,
            }),
        ),
        ControlCommand::CancelSession { session_id } => (session_id, SessionCommand::Cancel),
        ControlCommand::ReportGoal { session_id } => (session_id, SessionCommand::ReportGoal),
        ControlCommand::ApproveTool {
            session_id,
            call_id,
        } => (
            session_id,
            SessionCommand::Decide {
                call_id,
                approved: true,
            },
        ),
        ControlCommand::DenyTool {
            session_id,
            call_id,
            ..
        } => (
            session_id,
            SessionCommand::Decide {
                call_id,
                approved: false,
            },
        ),
        ControlCommand::ListSlashCommands => {
            list_slash_commands(context);
            return;
        }
        ControlCommand::CloseSession { session_id } => {
            // A session this run started ends itself; one it never started
            // has only what an earlier run kept.
            match sessions.remove(&session_id) {
                // A session that cannot take its close is stopped instead.
                Some(actor) => {
                    if actor.commands.try_send(SessionCommand::Close).is_err() {
                        actor.task.abort();
                    }
                }
                None => {
                    if let Err(e) = context.sessions.forget(session_id) {
                        tracing::warn!("session {session_id}: could not forget it: {e:#}");
                    }
                }
            }
            return;
        }
        other => {
            tracing::debug!("rc ignores {other:?}");
            return;
        }
    };
    // A refused input's answer is ended under its own name.
    let answer_id = match &message {
        SessionCommand::Input(input) => input.answer_id.clone(),
        _ => None,
    };
    if !sessions.contains_key(&session_id) && !room_for_one_more(sessions) {
        tracing::warn!("session {session_id}: refused, {MAX_LIVE_SESSIONS} sessions are open");
        report(
            context,
            session_id,
            answer_id,
            "This computer already has as many conversations open as it takes. Close one, or restart `sven rc`.",
        );
        return;
    }
    let actor = sessions
        .entry(session_id)
        .or_insert_with(|| Mailbox::start(session_id, context));
    match actor.commands.try_send(message) {
        Ok(()) => {}
        // Its actor ended (it panicked): this command starts it again.
        Err(mpsc::error::TrySendError::Closed(message)) => {
            tracing::warn!("session {session_id} had stopped; starting it again");
            let actor = Mailbox::start(session_id, context);
            let _ = actor.commands.try_send(message);
            sessions.insert(session_id, actor);
        }
        // It has not taken its last SESSION_QUEUE commands: it is wedged.
        // Waiting on it would hold up every other session's commands, so it
        // is stopped, its conversation told, and its next command starts it
        // again.
        Err(mpsc::error::TrySendError::Full(_)) => {
            if let Some(actor) = sessions.remove(&session_id) {
                actor.task.abort();
            }
            tracing::warn!("session {session_id} stopped taking its commands; stopped it");
            report_stopped(context, session_id, answer_id);
        }
    }
}

/// The most sessions one host runs at once. The server chooses session ids,
/// and each session holds an agent and may start MCP servers, so without a
/// bound a server could open them until the person's machine runs out.
const MAX_LIVE_SESSIONS: usize = 32;

/// Whether one more session may start, after forgetting those whose actor
/// has ended (a closed or finished conversation's).
fn room_for_one_more(sessions: &mut HashMap<Uuid, Mailbox>) -> bool {
    if sessions.len() >= MAX_LIVE_SESSIONS {
        sessions.retain(|_, actor| !actor.task.is_finished());
    }
    sessions.len() < MAX_LIVE_SESSIONS
}

/// Ends the waiting answer of a session the router had to stop, without
/// waiting on the uploader: the person sees why, and can send again.
fn report_stopped(context: &SessionContext, session_id: Uuid, answer_id: Option<String>) {
    report(
        context,
        session_id,
        answer_id,
        "This conversation stopped responding and was restarted. Send your last message again.",
    );
}

/// Ends the waiting answer of `session_id` with `message`, without waiting
/// on the uploader.
fn report(context: &SessionContext, session_id: Uuid, answer_id: Option<String>, message: &str) {
    let events = [
        ControlEvent::Session {
            session_id,
            event: AgentEvent::Error(message.into()),
            answer_id: answer_id.clone(),
        },
        ControlEvent::Session {
            session_id,
            event: AgentEvent::TurnComplete,
            answer_id,
        },
        ControlEvent::SessionState {
            session_id,
            state: SessionState::Idle,
        },
    ];
    for event in events {
        if context
            .uploads
            .try_send(Upload::Event(Box::new(event)))
            .is_err()
        {
            tracing::warn!("session {session_id}: the uploader is full; its stop is not reported");
            return;
        }
    }
}

/// A session's actor as the router sees it: where its commands go, and its
/// task, to stop it.
struct Mailbox {
    commands: mpsc::Sender<SessionCommand>,
    task: tokio::task::AbortHandle,
}

impl Mailbox {
    fn start(session_id: Uuid, context: &Arc<SessionContext>) -> Self {
        let (commands, rx) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(SessionActor::new(session_id, Arc::clone(context)).run(rx));
        Self {
            commands,
            task: task.abort_handle(),
        }
    }
}

/// Tells the server which slash commands a session here can run. The list
/// is read again each time: a command file added since shows up.
pub(super) fn list_slash_commands(context: &SessionContext) {
    let commands = slash::available(&context.working_dir);
    let event = ControlEvent::SlashCommands { commands };
    // Never waited on: the router calls this, and must not stall on uploads.
    if context
        .uploads
        .try_send(Upload::Event(Box::new(event)))
        .is_err()
    {
        tracing::warn!("the uploader is full or stopped; the slash command list is dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStore;

    type Uploaded = mpsc::Receiver<Upload>;

    fn test_context() -> (tempfile::TempDir, Arc<SessionContext>, Uploaded) {
        let dir = tempfile::tempdir().unwrap();
        let (uploads, uploaded) = mpsc::channel(64);
        let (asks, asks_open) = mpsc::channel(8);
        std::mem::forget(asks_open);
        let context = Arc::new(SessionContext {
            config: Arc::new({
                let mut config = sven_bootstrap::Config::default();
                config.model.provider = "mock".into();
                config.model.name = "mock-model".into();
                config
            }),
            working_dir: dir.path().to_path_buf(),
            hosted: false,
            wide_access: true,
            openai_base_url: String::new(),
            token: String::new(),
            uploads,
            sessions: SessionStore::new(dir.path().join("kept")),
            asks,
            mcp_tools_wait_ms: 0,
        });
        (dir, context, uploaded)
    }

    fn live_mailbox() -> Mailbox {
        let (commands, receiver) = mpsc::channel(SESSION_QUEUE);
        std::mem::forget(receiver);
        Mailbox {
            commands,
            task: tokio::spawn(std::future::pending::<()>()).abort_handle(),
        }
    }

    /// The server chooses session ids, and each session holds an agent (and
    /// may start MCP servers): a server cannot open more than
    /// [`MAX_LIVE_SESSIONS`] at once on a person's machine. One more is
    /// refused and its conversation told why; an ended one frees its place.
    #[tokio::test]
    async fn a_server_cannot_open_sessions_without_end() {
        let (_dir, context, mut uploaded) = test_context();
        let mut sessions: HashMap<Uuid, Mailbox> = (0..MAX_LIVE_SESSIONS)
            .map(|_| (Uuid::new_v4(), live_mailbox()))
            .collect();
        let refused = Uuid::new_v4();
        let open = |id| ControlCommand::CancelSession { session_id: id };
        route(open(refused), &mut sessions, &context).await;
        assert_eq!(sessions.len(), MAX_LIVE_SESSIONS);
        assert!(!sessions.contains_key(&refused));
        let mut told = false;
        while let Ok(Upload::Event(event)) = uploaded.try_recv() {
            told |= matches!(*event,
                ControlEvent::Session { session_id, event: AgentEvent::Error(_), .. } if session_id == refused);
        }
        assert!(told, "the refused conversation is told");

        // One ends; the next new session takes its place.
        let ended = *sessions.keys().next().unwrap();
        sessions.get(&ended).unwrap().task.abort();
        tokio::task::yield_now().await;
        let next = Uuid::new_v4();
        route(open(next), &mut sessions, &context).await;
        assert!(sessions.contains_key(&next) && !sessions.contains_key(&ended));
    }

    /// A session that stopped taking its commands holds up nobody else's:
    /// the router never waits on it. It is stopped, and its conversation
    /// told so, and the next command for it starts it again.
    #[tokio::test]
    async fn a_session_that_stopped_taking_commands_holds_up_no_other() {
        let (dir, context, mut uploaded) = test_context();
        let _ = &dir;
        // A session whose actor never reads its mailbox again.
        let wedged = Uuid::new_v4();
        let (commands, _never_read) = mpsc::channel(SESSION_QUEUE);
        let stuck = tokio::spawn(std::future::pending::<()>());
        let mut sessions = HashMap::from([(
            wedged,
            Mailbox {
                commands,
                task: stuck.abort_handle(),
            },
        )]);
        let cancel = |session_id| ControlCommand::CancelSession { session_id };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for _ in 0..=SESSION_QUEUE {
                route(cancel(wedged), &mut sessions, &context).await;
            }
            route(cancel(Uuid::new_v4()), &mut sessions, &context).await;
        })
        .await
        .expect("the router waited on a session that stopped taking commands");
        assert!(
            stuck.await.unwrap_err().is_cancelled(),
            "the wedged session is stopped"
        );
        assert!(
            !sessions.contains_key(&wedged),
            "its next command starts it again"
        );
        let mut told = Vec::new();
        while let Ok(Upload::Event(event)) = uploaded.try_recv() {
            told.push(*event);
        }
        assert!(
            told.iter().any(|event| matches!(event,
                ControlEvent::Session { session_id, event: AgentEvent::Error(_), .. } if *session_id == wedged)),
            "its conversation is told it stopped: {told:?}"
        );
    }
}
