// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! PTY ↔ WebSocket byte bridge.
//!
//! # Frame format
//!
//! All WebSocket frames are **binary**.
//!
//! | First byte | Meaning           | Remaining bytes              |
//! |------------|-------------------|------------------------------|
//! | `0x00`     | Terminal data     | Raw PTY output bytes         |
//! | `0x01`     | Control message   | UTF-8 JSON control object    |
//! | `0x02`     | PTY process exited| (no payload)                 |
//!
//! Client → server (browser → node):
//! - Frames without a `0x01` prefix = raw stdin to the PTY.
//! - `0x01` + JSON: `{"type":"resize","cols":N,"rows":M}`
//!
//! Server → client (node → browser):
//! - `0x00` + raw PTY bytes (stdout/stderr mix).
//! - `0x02` (single byte) = PTY process exited; client should show a
//!   "restart session" button instead of reconnecting automatically.

use std::io::Read as _;

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::pty::{manager::PtyManager, PtySession};

const TAG_CTRL: u8 = 0x01;
const TAG_PTY_EXITED: u8 = 0x02;

/// Control message from the browser.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ControlMsg {
    Resize { cols: u16, rows: u16 },
}

/// Run the PTY ↔ WebSocket bridge for one connected browser session.
pub async fn handle_pty_socket(
    socket: WebSocket,
    session: PtySession,
    manager: PtyManager,
    device_id: Uuid,
) {
    info!(device_id = %device_id, "PTY WebSocket session started");
    manager.mark_attached(device_id).await;

    // Destructure the session to move fields into separate tasks.
    let PtySession {
        id: _,
        stdin,
        reader,
        child,
        master,
    } = session;

    // Channel: PTY output bytes → WS sender task.
    let (pty_out_tx, mut pty_out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    // Signal from read_task → main loop when the PTY process has exited.
    let (pty_exit_tx, mut pty_exit_rx) = tokio::sync::oneshot::channel::<()>();

    // Spawn blocking PTY reader task.  Sends `None` payload via pty_exit_tx
    // when the read reaches EOF (process exited).
    let read_task = tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF = process exited
                Ok(n) => {
                    if pty_out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    debug!("PTY read error: {e}");
                    break;
                }
            }
        }
        // Signal PTY exit to the main loop.
        let _ = pty_exit_tx.send(());
    });

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Task: PTY output → WebSocket frames.
    let send_task = tokio::spawn(async move {
        while let Some(data) = pty_out_rx.recv().await {
            let mut frame = Vec::with_capacity(1 + data.len());
            frame.push(0x00u8); // TAG_DATA
            frame.extend_from_slice(&data);
            if ws_sender.send(Message::Binary(frame)).await.is_err() {
                break;
            }
        }
        // PTY output channel closed (process exited) — notify the browser.
        let _ = ws_sender.send(Message::Binary(vec![TAG_PTY_EXITED])).await;
        // Close with custom code 4001 so the browser knows this is a clean
        // process exit, not a network error requiring auto-reconnect.
        let _ = ws_sender
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 4001,
                reason: "process exited".into(),
            })))
            .await;
    });

    // Main receive loop: browser → PTY stdin (or control messages).
    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => {
                        if bytes.is_empty() {
                            continue;
                        }
                        if bytes[0] == TAG_CTRL {
                            if let Ok(text) = std::str::from_utf8(&bytes[1..]) {
                                match serde_json::from_str::<ControlMsg>(text) {
                                    Ok(ControlMsg::Resize { cols, rows }) => {
                                        if let Ok(m) = master.lock() {
                                            if let Err(e) = m.resize(portable_pty::PtySize {
                                                rows,
                                                cols,
                                                pixel_width: 0,
                                                pixel_height: 0,
                                            }) {
                                                warn!("PTY resize failed: {e}");
                                            } else {
                                                debug!(cols, rows, "PTY resized");
                                            }
                                        }
                                    }
                                    Err(e) => warn!("unknown control message: {e}"),
                                }
                            }
                        } else {
                            // Raw keyboard input → PTY stdin.
                            if let Ok(mut w) = stdin.lock() {
                                use std::io::Write;
                                if let Err(e) = w.write_all(&bytes) {
                                    warn!("PTY write failed: {e}");
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(mut w) = stdin.lock() {
                            use std::io::Write;
                            if let Err(e) = w.write_all(text.as_bytes()) {
                                warn!("PTY write (text frame) failed: {e}");
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Err(e)) => {
                        debug!(device_id = %device_id, "WebSocket error: {e}");
                        break;
                    }
                }
            }
            // PTY process exited — let the send_task deliver the exit
            // notification and close the socket; we just break out of the
            // receive loop.
            Ok(()) = &mut pty_exit_rx => {
                break;
            }
        }
    }

    read_task.abort();
    send_task.abort();

    // Check if the child has actually exited so the manager can clean up.
    let exited = child
        .lock()
        .map(|mut c| matches!(c.try_wait(), Ok(Some(_))))
        .unwrap_or(false);
    if exited {
        manager.remove(device_id).await;
    } else {
        manager.mark_detached(device_id).await;
    }

    info!(device_id = %device_id, "PTY WebSocket session ended");
}
