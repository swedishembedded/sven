// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! WhatsApp Business Cloud API channel adapter.
//!
//! Receives messages via webhook (`POST /channels/whatsapp`) and sends
//! replies via the WhatsApp Business Cloud API.
//!
//! # Setup
//!
//! 1. Create a Meta developer account and a Business App.
//! 2. Add WhatsApp product, get a phone number ID and access token.
//! 3. Configure the webhook in the Meta portal to point to:
//!    `https://<your-node>/channels/whatsapp`
//! 4. Set the verify token to match `verify_token` in your config.
//! 5. Configure sven:
//!    ```yaml
//!    channels:
//!      whatsapp:
//!        phone_number_id: "1234567890"
//!        access_token: "${WHATSAPP_TOKEN}"
//!        verify_token: "${WHATSAPP_VERIFY_TOKEN}"
//!    ```

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::info;

use crate::channel::{Channel, InboundMessage, OutboundMessage, ReplyContext};

/// Shared state for the WhatsApp webhook handler (used in HTTP router).
#[derive(Clone)]
pub struct WhatsAppWebhookState {
    pub phone_number_id: String,
    pub access_token: String,
    pub verify_token: String,
    pub tx: mpsc::Sender<InboundMessage>,
}

/// WhatsApp Business Cloud API channel adapter.
///
/// Inbound messages arrive via the webhook handler ([`WhatsAppWebhookState`]).
/// The `start()` method stores the sender for the webhook handler to use.
pub struct WhatsAppChannel {
    phone_number_id: String,
    access_token: String,
    verify_token: String,
    tx: Arc<Mutex<Option<mpsc::Sender<InboundMessage>>>>,
    client: reqwest::Client,
}

impl WhatsAppChannel {
    /// Create a new WhatsApp channel adapter.
    pub fn new(
        phone_number_id: impl Into<String>,
        access_token: impl Into<String>,
        verify_token: impl Into<String>,
    ) -> Self {
        Self {
            phone_number_id: phone_number_id.into(),
            access_token: access_token.into(),
            verify_token: verify_token.into(),
            tx: Arc::new(Mutex::new(None)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("WhatsApp HTTP client"),
        }
    }

    /// Get the webhook state for registering with the HTTP router.
    pub async fn webhook_state(&self) -> Option<WhatsAppWebhookState> {
        self.tx.lock().await.clone().map(|tx| WhatsAppWebhookState {
            phone_number_id: self.phone_number_id.clone(),
            access_token: self.access_token.clone(),
            verify_token: self.verify_token.clone(),
            tx,
        })
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        info!("WhatsApp channel started (waiting for webhook events)");
        *self.tx.lock().await = Some(tx);
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": msg.recipient,
            "type": "text",
            "text": { "body": msg.text }
        });

        self.client
            .post(format!(
                "https://graph.facebook.com/v18.0/{}/messages",
                self.phone_number_id
            ))
            .bearer_auth(&self.access_token)
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        *self.tx.lock().await = None;
        Ok(())
    }
}

/// Handle an inbound WhatsApp webhook notification.
///
/// Call this from the HTTP handler when `POST /channels/whatsapp` fires.
pub fn handle_webhook_payload(
    #[allow(unused_variables)] state: &WhatsAppWebhookState,
    payload: &serde_json::Value,
) -> Vec<InboundMessage> {
    let mut messages = Vec::new();

    let entries = match payload["entry"].as_array() {
        Some(e) => e,
        None => return messages,
    };

    for entry in entries {
        let changes = match entry["changes"].as_array() {
            Some(c) => c,
            None => continue,
        };
        for change in changes {
            let value = &change["value"];
            let msgs = match value["messages"].as_array() {
                Some(m) => m,
                None => continue,
            };
            for message in msgs {
                if message["type"].as_str() != Some("text") {
                    continue;
                }
                let sender = message["from"].as_str().unwrap_or("").to_string();
                let text = message["text"]["body"].as_str().unwrap_or("").to_string();
                let message_id = message["id"].as_str().map(|s| s.to_string());

                if text.is_empty() {
                    continue;
                }

                messages.push(InboundMessage {
                    channel: "whatsapp".to_string(),
                    sender: sender.clone(),
                    sender_name: None,
                    text,
                    attachments: vec![],
                    reply_context: ReplyContext {
                        message_id,
                        thread_id: Some(sender),
                    },
                });
            }
        }
    }

    messages
}
