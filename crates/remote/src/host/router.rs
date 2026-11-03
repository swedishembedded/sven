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
        ControlCommand::SendInput { session_id, text } => (session_id, SessionCommand::Input(text)),
        ControlCommand::CancelSession { session_id } => (session_id, SessionCommand::Cancel),
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
            report_stopped(context, session_id);
        }
    }
}

/// Ends the waiting answer of a session the router had to stop, without
/// waiting on the uploader: the person sees why, and can send again.
fn report_stopped(context: &SessionContext, session_id: Uuid) {
    let events = [
        ControlEvent::Session {
            session_id,
            event: AgentEvent::Error(
                "This conversation stopped responding and was restarted. Send your last message again."
                    .into(),
            ),
        },
        ControlEvent::Session {
            session_id,
            event: AgentEvent::TurnComplete,
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

    /// A session that stopped taking its commands holds up nobody else's:
    /// the router never waits on it. It is stopped, and its conversation
    /// told so, and the next command for it starts it again.
    #[tokio::test]
    async fn a_session_that_stopped_taking_commands_holds_up_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let (uploads, mut uploaded) = mpsc::channel(64);
        let (asks, _asks_open) = mpsc::channel(8);
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
                ControlEvent::Session { session_id, event: AgentEvent::Error(_) } if *session_id == wedged)),
            "its conversation is told it stopped: {told:?}"
        );
    }
}
