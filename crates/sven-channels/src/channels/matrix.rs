// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Matrix homeserver channel adapter.
//!
//! Uses the Matrix Client-Server API (v3) over HTTPS for receiving and
//! sending messages. Supports login with username/password; the access
//! token is cached in memory for the session lifetime.
//!
//! # Setup
//!
//! 1. Register a bot account on your Matrix homeserver.
//! 2. Configure sven:
//!    ```yaml
//!    channels:
//!      matrix:
//!        homeserver: "https://matrix.org"
//!        username: "@sven:matrix.org"
//!        password: "${MATRIX_PASSWORD}"
//!        room_ids: ["!roomid:matrix.org"]
//!    ```

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::channel::{Channel, InboundMessage, OutboundMessage, ReplyContext};

struct MatrixSession {
    access_token: String,
    #[allow(dead_code)]
    device_id: String,
}

/// Matrix Client-Server API channel adapter.
pub struct MatrixChannel {
    homeserver: String,
    username: String,
    password: String,
    room_ids: Vec<String>,
    session: Arc<Mutex<Option<MatrixSession>>>,
    next_batch: Arc<Mutex<Option<String>>>,
    client: reqwest::Client,
}

impl MatrixChannel {
    /// Create a new Matrix channel adapter.
    pub fn new(
        homeserver: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
        room_ids: Vec<String>,
    ) -> Self {
        Self {
            homeserver: homeserver.into().trim_end_matches('/').to_string(),
            username: username.into(),
            password: password.into(),
            room_ids,
            session: Arc::new(Mutex::new(None)),
            next_batch: Arc::new(Mutex::new(None)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(35))
                .build()
                .expect("Matrix HTTP client"),
        }
    }

    async fn login(&self) -> anyhow::Result<String> {
        let payload = serde_json::json!({
            "type": "m.login.password",
            "identifier": { "type": "m.id.user", "user": self.username },
            "password": self.password,
            "initial_device_display_name": "sven-agent"
        });

        let resp: serde_json::Value = self
            .client
            .post(format!("{}/_matrix/client/v3/login", self.homeserver))
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        let token = resp["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Matrix login failed: {resp}"))?
            .to_string();

        let device_id = resp["device_id"].as_str().unwrap_or("sven").to_string();

        *self.session.lock().await = Some(MatrixSession {
            access_token: token.clone(),
            device_id,
        });

        Ok(token)
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        info!(homeserver = %self.homeserver, "Matrix channel starting");

        // Login to get access token
        let token = self.login().await?;

        let homeserver = self.homeserver.clone();
        let room_ids = self.room_ids.clone();
        let next_batch = self.next_batch.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            loop {
                let batch = next_batch.lock().await.clone();

                let mut req = client.get(format!("{homeserver}/_matrix/client/v3/sync"));

                req = req.bearer_auth(&token).query(&[("timeout", "30000")]);
                if let Some(batch) = &batch {
                    req = req.query(&[("since", batch.as_str())]);
                }

                let resp: serde_json::Value = match req.send().await {
                    Ok(r) => r.json().await.unwrap_or_default(),
                    Err(e) => {
                        error!(error = %e, "Matrix: sync error — retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                // Update next_batch
                if let Some(nb) = resp["next_batch"].as_str() {
                    *next_batch.lock().await = Some(nb.to_string());
                }

                // Process timeline events for each joined room
                if let Some(rooms) = resp["rooms"]["join"].as_object() {
                    for (room_id, room_data) in rooms {
                        if !room_ids.is_empty() && !room_ids.contains(room_id) {
                            continue;
                        }

                        let events = match room_data["timeline"]["events"].as_array() {
                            Some(e) => e,
                            None => continue,
                        };

                        for event in events {
                            if event["type"].as_str() != Some("m.room.message") {
                                continue;
                            }

                            let content = &event["content"];
                            if content["msgtype"].as_str() != Some("m.text") {
                                continue;
                            }

                            let text = content["body"].as_str().unwrap_or("").to_string();
                            if text.is_empty() {
                                continue;
                            }

                            let sender = event["sender"].as_str().unwrap_or("").to_string();
                            let event_id = event["event_id"].as_str().map(|s| s.to_string());

                            let inbound = InboundMessage {
                                channel: "matrix".to_string(),
                                sender,
                                sender_name: None,
                                text,
                                attachments: vec![],
                                reply_context: ReplyContext {
                                    message_id: event_id,
                                    thread_id: Some(room_id.clone()),
                                },
                            };

                            if tx.send(inbound).await.is_err() {
                                warn!("Matrix: inbound channel closed — stopping");
                                return;
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()> {
        let token = {
            let session = self.session.lock().await;
            session
                .as_ref()
                .map(|s| s.access_token.clone())
                .ok_or_else(|| anyhow::anyhow!("Matrix: not logged in"))?
        };

        let room_id = msg
            .reply_context
            .as_ref()
            .and_then(|c| c.thread_id.as_deref())
            .unwrap_or(&msg.recipient);

        let txn_id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "msgtype": "m.text",
            "body": msg.text
        });

        self.client
            .put(format!(
                "{}/_matrix/client/v3/rooms/{room_id}/send/m.room.message/{txn_id}",
                self.homeserver
            ))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        info!("Matrix channel stopping");
        Ok(())
    }
}
