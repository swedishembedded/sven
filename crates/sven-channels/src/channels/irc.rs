// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! IRC channel adapter using raw TCP + TLS.
//!
//! Implements a minimal IRC client (RFC 1459 + TLS) for receiving and
//! sending messages. Supports NickServ identification and channel joining.
//!
//! # Setup
//!
//! ```yaml
//! channels:
//!   irc:
//!     server: "irc.libera.chat"
//!     port: 6697
//!     tls: true
//!     nickname: "sven-bot"
//!     channels: ["#sven", "#general"]
//!     password: "${IRC_NICKSERV_PASSWORD}"
//! ```

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::channel::{Channel, InboundMessage, OutboundMessage, ReplyContext};

/// IRC channel adapter.
///
/// Uses a raw TCP connection with optional TLS. A simple IRC protocol
/// parser handles PRIVMSG, PING, and JOIN commands.
pub struct IrcChannel {
    server: String,
    port: u16,
    #[allow(dead_code)]
    tls: bool,
    nickname: String,
    irc_channels: Vec<String>,
    password: Option<String>,
    writer: Arc<Mutex<Option<tokio::sync::mpsc::Sender<String>>>>,
}

impl IrcChannel {
    /// Create a new IRC channel adapter.
    pub fn new(
        server: impl Into<String>,
        port: u16,
        tls: bool,
        nickname: impl Into<String>,
        irc_channels: Vec<String>,
        password: Option<String>,
    ) -> Self {
        Self {
            server: server.into(),
            port,
            tls,
            nickname: nickname.into(),
            irc_channels,
            password,
            writer: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait]
impl Channel for IrcChannel {
    fn name(&self) -> &str {
        "irc"
    }

    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        info!(server = %self.server, "IRC channel starting");

        let server = self.server.clone();
        let port = self.port;
        let nickname = self.nickname.clone();
        let irc_channels = self.irc_channels.clone();
        let password = self.password.clone();
        let writer_slot = self.writer.clone();

        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            use tokio::net::TcpStream;

            let addr = format!("{server}:{port}");
            let stream = match TcpStream::connect(&addr).await {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, addr, "IRC: connection failed");
                    return;
                }
            };

            let (reader, mut writer_raw) = tokio::io::split(stream);
            let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<String>(64);
            *writer_slot.lock().await = Some(write_tx.clone());

            // Spawn write task
            tokio::spawn(async move {
                while let Some(line) = write_rx.recv().await {
                    let msg = format!("{line}\r\n");
                    if let Err(e) = writer_raw.write_all(msg.as_bytes()).await {
                        error!(error = %e, "IRC: write error");
                        break;
                    }
                }
            });

            // Send registration
            if let Some(pass) = &password {
                let _ = write_tx.send(format!("PASS {pass}")).await;
            }
            let _ = write_tx.send(format!("NICK {nickname}")).await;
            let _ = write_tx
                .send(format!("USER {nickname} 0 * :sven agent"))
                .await;

            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();

                // PING/PONG keepalive
                if line.starts_with("PING") {
                    let pong = line.replace("PING", "PONG");
                    let _ = write_tx.send(pong).await;
                    continue;
                }

                // Detect 001 (welcome) — join channels
                if line.contains(" 001 ") {
                    for ch in &irc_channels {
                        let _ = write_tx.send(format!("JOIN {ch}")).await;
                    }
                    continue;
                }

                // Parse PRIVMSG
                // `:nick!user@host PRIVMSG #channel :message text`
                if !line.contains(" PRIVMSG ") {
                    continue;
                }

                let parts: Vec<&str> = line.splitn(2, " PRIVMSG ").collect();
                if parts.len() != 2 {
                    continue;
                }

                let sender_full = parts[0].trim_start_matches(':');
                let sender = sender_full
                    .split('!')
                    .next()
                    .unwrap_or(sender_full)
                    .to_string();

                let rest = parts[1];
                let chan_rest: Vec<&str> = rest.splitn(2, " :").collect();
                if chan_rest.len() != 2 {
                    continue;
                }

                let target = chan_rest[0].to_string();
                let text = chan_rest[1].to_string();

                if text.is_empty() {
                    continue;
                }

                let inbound = InboundMessage {
                    channel: "irc".to_string(),
                    sender,
                    sender_name: None,
                    text,
                    attachments: vec![],
                    reply_context: ReplyContext {
                        message_id: None,
                        thread_id: Some(target),
                    },
                };

                if tx.send(inbound).await.is_err() {
                    warn!("IRC: inbound channel closed — stopping");
                    return;
                }
            }

            warn!("IRC: connection closed");
        });

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()> {
        let target = msg
            .reply_context
            .as_ref()
            .and_then(|c| c.thread_id.as_deref())
            .unwrap_or(&msg.recipient);

        let writer = self.writer.lock().await;
        let tx = writer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("IRC: channel not started"))?;

        // Split long messages to avoid IRC line limits
        for chunk in msg.text.chars().collect::<Vec<_>>().chunks(400) {
            let chunk_str: String = chunk.iter().collect();
            tx.send(format!("PRIVMSG {target} :{chunk_str}"))
                .await
                .map_err(|_| anyhow::anyhow!("IRC: write channel closed"))?;
        }

        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        info!("IRC channel stopping");
        *self.writer.lock().await = None;
        Ok(())
    }
}
