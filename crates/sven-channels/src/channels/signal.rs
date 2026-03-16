// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Signal channel adapter via `signal-cli` subprocess.
//!
//! Uses the `signal-cli` JSON-RPC daemon mode for full send/receive support.
//!
//! # Setup
//!
//! 1. Install `signal-cli` from https://github.com/AsamK/signal-cli/releases
//! 2. Register or link your phone number:
//!    ```sh
//!    signal-cli -u +1234567890 register
//!    signal-cli -u +1234567890 verify <code>
//!    ```
//! 3. Configure sven:
//!    ```yaml
//!    channels:
//!      signal:
//!        signal_cli_path: "/usr/local/bin/signal-cli"
//!        phone_number: "+1234567890"
//!    ```

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::channel::{Channel, InboundMessage, OutboundMessage, ReplyContext};

/// Signal channel adapter via `signal-cli` JSON-RPC daemon.
pub struct SignalChannel {
    cli_path: String,
    phone_number: String,
}

impl SignalChannel {
    /// Create a new Signal channel adapter.
    pub fn new(cli_path: impl Into<String>, phone_number: impl Into<String>) -> Self {
        Self {
            cli_path: cli_path.into(),
            phone_number: phone_number.into(),
        }
    }
}

#[async_trait]
impl Channel for SignalChannel {
    fn name(&self) -> &str {
        "signal"
    }

    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        info!(number = %self.phone_number, "Signal channel starting");

        let cli_path = self.cli_path.clone();
        let phone_number = self.phone_number.clone();

        tokio::spawn(async move {
            // Use signal-cli in JSON output mode for receive
            let mut child = match tokio::process::Command::new(&cli_path)
                .args([
                    "-u",
                    &phone_number,
                    "--output=json",
                    "receive",
                    "--timeout=-1",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    error!(error = %e, "Signal: failed to start signal-cli");
                    return;
                }
            };

            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    error!("Signal: could not get signal-cli stdout");
                    return;
                }
            };

            let mut lines = BufReader::new(stdout).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let json: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Parse signal-cli JSON receive output
                let envelope = &json["envelope"];
                if envelope.is_null() {
                    continue;
                }

                let data_message = &envelope["dataMessage"];
                if data_message.is_null() {
                    continue;
                }

                let text = match data_message["message"].as_str() {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => continue,
                };

                let sender = envelope["sourceNumber"].as_str().unwrap_or("").to_string();

                let sender_name = envelope["sourceName"].as_str().map(|s| s.to_string());

                let inbound = InboundMessage {
                    channel: "signal".to_string(),
                    sender: sender.clone(),
                    sender_name,
                    text,
                    attachments: vec![],
                    reply_context: ReplyContext {
                        message_id: None,
                        thread_id: Some(sender),
                    },
                };

                if tx.send(inbound).await.is_err() {
                    warn!("Signal: inbound channel closed — stopping");
                    let _ = child.kill().await;
                    return;
                }
            }

            warn!("Signal: signal-cli process ended");
        });

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()> {
        let output = tokio::process::Command::new(&self.cli_path)
            .args([
                "-u",
                &self.phone_number,
                "send",
                "-m",
                &msg.text,
                &msg.recipient,
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("signal-cli send failed: {}", stderr);
        }

        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        info!("Signal channel stopping");
        Ok(())
    }
}
