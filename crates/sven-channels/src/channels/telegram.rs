// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Telegram Bot API channel adapter.
//!
//! Uses the Telegram Bot API via long-polling or webhooks.
//! Enable the `telegram` Cargo feature to activate the `teloxide` backend.
//! Without the feature, a minimal HTTP polling implementation is used.
//!
//! # Setup
//!
//! 1. Create a bot with [@BotFather](https://t.me/BotFather) and copy the token.
//! 2. Configure sven:
//!    ```yaml
//!    channels:
//!      telegram:
//!        bot_token: "${TELEGRAM_BOT_TOKEN}"
//!        allowed_users: []   # empty = allow all
//!    ```
//! 3. Start the node: `sven node start`
//! 4. Message your bot — sven will respond.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use crate::channel::{Channel, InboundMessage, OutboundMessage, ReplyContext};

/// Telegram update offset for long-polling.
type UpdateOffset = i64;

/// Telegram channel adapter.
///
/// Uses the Telegram Bot API over HTTPS long-polling.
pub struct TelegramChannel {
    bot_token: String,
    allowed_users: Vec<i64>,
    offset: Arc<Mutex<UpdateOffset>>,
    client: reqwest::Client,
}

impl TelegramChannel {
    /// Create a new Telegram channel adapter.
    ///
    /// `allowed_users` is the list of Telegram user IDs permitted to interact
    /// with the agent. An empty list means all users are permitted.
    pub fn new(bot_token: impl Into<String>, allowed_users: Vec<i64>) -> Self {
        Self {
            bot_token: bot_token.into(),
            allowed_users,
            offset: Arc::new(Mutex::new(0)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(35))
                .build()
                .expect("Telegram HTTP client"),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.bot_token, method)
    }

    #[allow(dead_code)]
    fn is_allowed(&self, user_id: i64) -> bool {
        self.allowed_users.is_empty() || self.allowed_users.contains(&user_id)
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        info!("Telegram channel starting (long-polling)");

        let bot_token = self.bot_token.clone();
        let allowed_users = self.allowed_users.clone();
        let offset = self.offset.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            loop {
                let current_offset = *offset.lock().await;
                let url = format!("https://api.telegram.org/bot{}/getUpdates", bot_token);
                let result = client
                    .get(&url)
                    .query(&[
                        ("offset", current_offset.to_string()),
                        ("timeout", "30".to_string()),
                        ("allowed_updates", r#"["message"]"#.to_string()),
                    ])
                    .send()
                    .await;

                match result {
                    Err(e) => {
                        error!(error = %e, "Telegram getUpdates error — retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                    Ok(resp) => {
                        let body: Value = match resp.json().await {
                            Ok(v) => v,
                            Err(e) => {
                                error!(error = %e, "Telegram response parse error");
                                continue;
                            }
                        };

                        let updates = match body["result"].as_array() {
                            Some(arr) => arr.clone(),
                            None => continue,
                        };

                        for update in &updates {
                            let update_id = update["update_id"].as_i64().unwrap_or(0);
                            *offset.lock().await = update_id + 1;

                            if let Some(msg) = update.get("message") {
                                let user_id = msg["from"]["id"].as_i64().unwrap_or(0);
                                if !allowed_users.is_empty() && !allowed_users.contains(&user_id) {
                                    debug!(
                                        user_id,
                                        "Telegram: ignoring message from non-allowed user"
                                    );
                                    continue;
                                }

                                let text = msg["text"].as_str().unwrap_or("").to_string();
                                if text.is_empty() {
                                    continue;
                                }

                                let sender = user_id.to_string();
                                let sender_name = msg["from"]["username"]
                                    .as_str()
                                    .or_else(|| msg["from"]["first_name"].as_str())
                                    .map(|s| s.to_string());

                                let chat_id = msg["chat"]["id"].as_i64().unwrap_or(0).to_string();
                                let message_id =
                                    msg["message_id"].as_i64().map(|id| id.to_string());

                                let inbound = InboundMessage {
                                    channel: "telegram".to_string(),
                                    sender,
                                    sender_name,
                                    text,
                                    attachments: vec![],
                                    reply_context: ReplyContext {
                                        message_id,
                                        thread_id: Some(chat_id),
                                    },
                                };

                                if tx.send(inbound).await.is_err() {
                                    warn!("Telegram: inbound channel closed — stopping");
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()> {
        // The recipient for Telegram is the chat_id (thread_id from ReplyContext).
        let chat_id = if let Some(ctx) = &msg.reply_context {
            ctx.thread_id.clone().unwrap_or(msg.recipient.clone())
        } else {
            msg.recipient.clone()
        };

        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text": msg.text,
            "parse_mode": "Markdown",
        });

        self.client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        info!("Telegram channel stopping");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_format() {
        let ch = TelegramChannel::new("test-token", vec![]);
        assert_eq!(
            ch.api_url("getMe"),
            "https://api.telegram.org/bottest-token/getMe"
        );
    }

    #[test]
    fn allowed_users_empty_allows_all() {
        let ch = TelegramChannel::new("token", vec![]);
        assert!(ch.is_allowed(12345));
        assert!(ch.is_allowed(99999));
    }

    #[test]
    fn allowed_users_filters_correctly() {
        let ch = TelegramChannel::new("token", vec![100, 200]);
        assert!(ch.is_allowed(100));
        assert!(ch.is_allowed(200));
        assert!(!ch.is_allowed(300));
    }
}
