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
//!
//! Client → server (browser → node):
//! - Frames without a `0x01` prefix = raw stdin to the PTY.
//! - `0x01` + JSON: `{"type":"resize","cols":N,"rows":M}`
//!
//! Server → client (node → browser):
//! - `0x00` + raw PTY bytes (stdout/stderr mix).

use std::io::Read as _;

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::pty::{manager::PtyManager, PtySession};

const TAG_CTRL: u8 = 0x01;

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
        mut stdin,
        reader,
        child: _child,
        master,
    } = session;

    // Channel: PTY output bytes → WS sender task.
    let (pty_out_tx, mut pty_out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    // Spawn blocking PTY reader task.
    let read_task = tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
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
    });

    // Main receive loop: browser → PTY stdin (or control messages).
    loop {
        match ws_receiver.next().await {
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
                    // Raw keyboard input.
                    use std::io::Write;
                    if let Err(e) = stdin.write_all(&bytes) {
                        warn!("PTY write failed: {e}");
                        break;
                    }
                }
            }
            Some(Ok(Message::Text(text))) => {
                use std::io::Write;
                if let Err(e) = stdin.write_all(text.as_bytes()) {
                    warn!("PTY write (text frame) failed: {e}");
                    break;
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

    read_task.abort();
    send_task.abort();
    manager.mark_detached(device_id).await;
    info!(device_id = %device_id, "PTY WebSocket session ended");
}
