// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! In-process **one-tap share bridge** — expose the *already-running* local
//! kernel session to a broker so a remote consultant can steer it.
//!
//! Where `sven-cloud`'s `run_share_bridge` builds a fresh kernel and reuses the
//! node's `ControlService` (and therefore pulls in `sven-node`'s heavy
//! libp2p/axum stack), this bridge is the *interactive* path: typing `/share`
//! in the running TUI hands **this** session's live
//! [`RuntimeHandle`](sven_bootstrap::RuntimeHandle) to the broker directly — no
//! second kernel, no copy-paste handoff.
//!
//! It reuses the transport-agnostic mappings from `sven-control`
//! ([`ui_event_to_control`] up, [`control_command_to_kernel_event`] down), so
//! `sven-frontend` never depends on `sven-node`:
//!
//! ```text
//!   running kernel (RuntimeHandle)          broker (GET /share)          consultant
//!         │                                     │                          │
//!  subscribe_observations() ─► ui_event_to_control ─► ShareFrame::Event ──►│
//!         │  ◄── control_command_to_kernel_event ◄── ShareFrame::Command ◄─│
//!  handle.sink().emit(Event) ◄──┘
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures::{SinkExt as _, StreamExt as _};
use sven_bootstrap::RuntimeHandle;
use sven_control::{
    control_command_session_id, control_command_to_kernel_event, ui_event_to_control,
    ControlCommand,
};
use sven_node_client::ConnectOptions;
use sven_wire::{ShareFrame, ShareRegister};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

/// Heartbeat cadence towards the broker. Must stay comfortably under the
/// broker's read-idle timeout (90 s) so an idle-but-alive share is not evicted
/// between consultant prompts.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(25);

/// How to reach the broker and announce the shared session.
#[derive(Debug, Clone)]
pub struct FrontendShareOptions {
    /// Broker base URL (e.g. `https://localhost:8443`) or an explicit
    /// `wss://…/share` URL. `https`/`http` are rewritten to `wss`/`ws` and
    /// `/share` is appended when absent.
    pub broker_url: String,
    /// Tenant bearer secret (any valid token of the tenant). The broker takes
    /// the tenant from this token.
    pub token: String,
    /// Stable id a consultant attaches by (`GET /share/:share_id`).
    pub share_id: String,
    /// Tenant that owns the share. Must match the token's tenant or the broker
    /// closes the connection.
    pub tenant_id: String,
    /// Human-readable label shown to the consultant.
    pub title: String,
    /// Extra CA PEM to trust (self-signed control planes).
    pub ca_pem: Option<PathBuf>,
    /// Disable TLS verification — local testing only.
    pub insecure_dev: bool,
}

/// Bridge the already-running kernel `handle` onto the broker's `/share`
/// endpoint and run until the connection closes.
///
/// `ready_tx`, when supplied, fires with the `share_id` immediately after the
/// register frame is accepted so the caller can print "Session shared as <id>"
/// only once the share is actually live.
///
/// # Errors
///
/// Fails on a WebSocket dial/TLS error or if the register frame cannot be
/// written. A clean socket close returns `Ok(())`.
pub async fn run_frontend_share_bridge(
    handle: RuntimeHandle,
    opts: FrontendShareOptions,
    ready_tx: Option<tokio::sync::oneshot::Sender<String>>,
) -> Result<()> {
    // ── Dial the broker's /share endpoint ────────────────────────────────────
    let ws_url = share_ws_url(&opts.broker_url);
    let connect_options = ConnectOptions {
        extra_ca_pem: opts.ca_pem.clone(),
        insecure_dev: opts.insecure_dev,
    };
    let stream = sven_node_client::connect_with_options(&ws_url, &opts.token, &connect_options)
        .await
        .with_context(|| format!("connecting to {ws_url}"))?;
    let (mut ws_tx, mut ws_rx) = stream.split();

    // ── First frame: the register (raw ShareRegister, not a ShareFrame) ──────
    let register = ShareRegister {
        share_id: opts.share_id.clone(),
        tenant_id: opts.tenant_id.clone(),
        title: opts.title.clone(),
    };
    let register_json =
        serde_json::to_string(&register).context("serializing the share register")?;
    ws_tx
        .send(WsMessage::Text(register_json))
        .await
        .context("sending the share register frame")?;
    tracing::info!(
        share_id = %opts.share_id,
        tenant_id = %opts.tenant_id,
        "one-tap share: registered with the broker; waiting for a consultant"
    );
    if let Some(ready_tx) = ready_tx {
        let _ = ready_tx.send(opts.share_id.clone());
    }

    // ── Wire the transport-agnostic translation core ─────────────────────────
    // `inbound`  : parsed ShareFrames from the socket → translation.
    // `outbound` : ShareFrame::Event(s) from the translation → the socket.
    let (inbound_tx, inbound_rx) = mpsc::channel::<ShareFrame>(256);
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<ShareFrame>(256);
    let translation = tokio::spawn(run_share_translation(handle, inbound_rx, outbound_tx));

    // ── Up: outbound Event frames → socket (+ heartbeats) ────────────────────
    let writer = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
        ticker.tick().await; // fire-immediately tick; skip it
        loop {
            let frame = tokio::select! {
                frame = outbound_rx.recv() => match frame {
                    Some(frame) => frame,
                    None => break, // translation ended
                },
                _ = ticker.tick() => ShareFrame::Heartbeat,
            };
            let json = match serde_json::to_string(&frame) {
                Ok(json) => json,
                Err(e) => {
                    tracing::warn!(error = %e, "one-tap share: failed to serialize outbound frame");
                    continue;
                }
            };
            if ws_tx.send(WsMessage::Text(json)).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.send(WsMessage::Close(None)).await;
    });

    // ── Down: inbound socket frames → translation ────────────────────────────
    while let Some(msg) = ws_rx.next().await {
        let text = match msg.context("one-tap share: socket error")? {
            WsMessage::Text(text) => text,
            WsMessage::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            WsMessage::Close(_) => break,
            _ => continue, // ping/pong handled by the stack
        };
        let Ok(frame) = serde_json::from_str::<ShareFrame>(&text) else {
            continue;
        };
        if inbound_tx.send(frame).await.is_err() {
            break; // translation task ended
        }
    }

    // Dropping `inbound_tx` ends the translation loop, which aborts its up-task.
    drop(inbound_tx);
    writer.abort();
    let _ = writer.await;
    let _ = translation.await;
    tracing::info!(share_id = %opts.share_id, "one-tap share: disconnected");
    Ok(())
}

/// The transport-agnostic translation core: drives the live kernel from inbound
/// [`ShareFrame`]s and forwards its observations back as outbound
/// [`ShareFrame::Event`]s. Exposed (crate-internal) so it can be exercised
/// in-process without a real WebSocket.
///
/// * **down** — [`ShareFrame::Command`] → [`control_command_to_kernel_event`] →
///   `handle.sink().emit(..)`. The session id carried by each command is tracked
///   so outbound events are tagged with the id the consultant filters on.
/// * **up** — kernel [`UiEvent`](sven_hsm::UiEvent) → [`ui_event_to_control`] →
///   `ShareFrame::Event`.
///
/// Runs until `inbound` closes or the kernel event queue closes.
pub(crate) async fn run_share_translation(
    handle: RuntimeHandle,
    mut inbound: mpsc::Receiver<ShareFrame>,
    outbound: mpsc::Sender<ShareFrame>,
) {
    use tokio::sync::broadcast::error::RecvError;

    // The session id outbound events are tagged with. Seeded with a fresh id so
    // a read-only consultant still gets well-formed events; updated to match
    // each inbound command's target so a steering consultant (which filters by
    // its own session UUID) receives the stream.
    let session_id = Arc::new(Mutex::new(Uuid::new_v4()));

    // ── Up-task: observations → ShareFrame::Event ────────────────────────────
    let mut obs_rx = handle.subscribe_observations();
    let up_session_id = Arc::clone(&session_id);
    let up_out = outbound.clone();
    let up = tokio::spawn(async move {
        loop {
            match obs_rx.recv().await {
                Ok(ui) => {
                    let sid = *up_session_id.lock().unwrap();
                    let Some(ctrl) = ui_event_to_control(ui, sid) else {
                        continue;
                    };
                    let Ok(value) = serde_json::to_value(&ctrl) else {
                        continue;
                    };
                    if up_out.send(ShareFrame::Event(value)).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    // ── Down-loop: inbound Command frames → kernel ───────────────────────────
    let sink = handle.sink();
    while let Some(frame) = inbound.recv().await {
        let ShareFrame::Command(value) = frame else {
            continue; // events/heartbeats coming the wrong way: ignore
        };
        let Ok(command) = serde_json::from_value::<ControlCommand>(value) else {
            continue;
        };
        if let Some(sid) = control_command_session_id(&command) {
            *session_id.lock().unwrap() = sid;
        }
        if let Some(event) = control_command_to_kernel_event(&command) {
            if !sink.emit(event).await {
                break; // kernel queue closed
            }
        }
    }

    up.abort();
    let _ = up.await;
}

/// Derives the `/share` WebSocket URL from a broker base URL: `http`→`ws`,
/// `https`→`wss`, then append `/share` when it is not already present.
fn share_ws_url(base: &str) -> String {
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    let ws = ws.trim_end_matches('/');
    if ws.ends_with("/share") {
        ws.to_string()
    } else {
        format!("{ws}/share")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use sven_bootstrap::RuntimeBuilder;
    use sven_control::ControlEvent;
    use sven_model::ScriptedMockProvider;
    use sven_wire::ShareFrame;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn derives_share_wss_url() {
        assert_eq!(
            share_ws_url("https://localhost:8443"),
            "wss://localhost:8443/share"
        );
        assert_eq!(
            share_ws_url("http://127.0.0.1:9000/"),
            "ws://127.0.0.1:9000/share"
        );
        assert_eq!(
            share_ws_url("wss://cloud.example.com/share"),
            "wss://cloud.example.com/share"
        );
    }

    /// The one-tap in-process bridge round-trips against a **live** kernel: a
    /// `ShareFrame::Command(SendInput)` reaches the kernel (it drives a real
    /// turn on the mock provider) AND that turn's observations come back up as
    /// `ShareFrame::Event`s — proving both directions of the interactive path
    /// without any WebSocket.
    #[tokio::test]
    async fn share_translation_round_trips_command_and_event() {
        // ── Build a live kernel session driven by a deterministic mock. ──────
        let mut config = sven_config::Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        let bundle = RuntimeBuilder::new(Arc::new(config), "chat")
            .with_model_provider(Box::new(ScriptedMockProvider::always_text(
                "shared reply from the local session",
            )))
            .build_session()
            .await
            .expect("kernel session should build with the mock provider");

        let handle = bundle.handle.clone();

        // Auto-consume kernel approval/question gates so the mock turn runs to
        // completion unattended (mirrors the node / `sven share` wiring).
        let mut channels = bundle.channels;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    q = channels.question_rx.recv() => match q {
                        Some(q) => { let _ = q.reply_tx.send(String::new()); }
                        None => break,
                    },
                    a = channels.approval_rx.recv() => match a {
                        Some(a) => { let _ = a.reply_tx.send(true); }
                        None => break,
                    },
                }
            }
        });
        // Keep the runtime alive for the duration of the test.
        let _runtime = bundle.runtime;

        // ── Attach the translation core via in-memory channels. ──────────────
        let (inbound_tx, inbound_rx) = mpsc::channel::<ShareFrame>(16);
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<ShareFrame>(64);
        let translation =
            tokio::spawn(run_share_translation(handle, inbound_rx, outbound_tx));

        // ── Down: a consultant's SendInput, wrapped as a Command frame. ──────
        let session_id = Uuid::new_v4();
        let command = ControlCommand::SendInput {
            session_id,
            text: "please answer".into(),
        };
        let frame = ShareFrame::Command(serde_json::to_value(&command).unwrap());
        inbound_tx.send(frame).await.unwrap();

        // ── Up: the resulting observations must surface as Event frames that
        //        carry the consultant's session id and the mock reply text. ───
        let mut saw_output = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, outbound_rx.recv()).await {
            let ShareFrame::Event(value) = frame else {
                continue; // heartbeats etc.
            };
            let Ok(event) = serde_json::from_value::<ControlEvent>(value) else {
                continue;
            };
            match event {
                ControlEvent::OutputDelta {
                    session_id: sid,
                    delta,
                    ..
                } => {
                    assert_eq!(sid, session_id, "event must carry the consultant's session id");
                    if delta.contains("shared reply") {
                        saw_output = true;
                    }
                }
                ControlEvent::OutputComplete {
                    session_id: sid,
                    text,
                    ..
                } => {
                    assert_eq!(sid, session_id);
                    if text.contains("shared reply") {
                        saw_output = true;
                    }
                }
                ControlEvent::SessionState {
                    session_id: sid, ..
                } => {
                    assert_eq!(sid, session_id);
                    break; // turn completed
                }
                _ => continue,
            }
        }

        assert!(
            saw_output,
            "the SendInput command must reach the kernel and its reply must \
             flow back up as a ShareFrame::Event carrying session {session_id}"
        );

        drop(inbound_tx);
        let _ = translation.await;
    }
}
