// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Discord Bot channel adapter.
//!
//! Uses the Discord HTTP API + Gateway for receiving messages.
//! This implementation uses Discord's REST API for message polling and
//! sending. For full real-time support, enable the `discord` Cargo feature
//! to use the serenity Gateway backend.
//!
//! # Setup
//!
//! 1. Create a Discord application at https://discord.com/developers/applications
//! 2. Add a Bot and copy the bot token.
//! 3. Enable "Message Content Intent" in the Bot settings.
//! 4. Invite the bot to your server using the OAuth2 URL generator.
//! 5. Configure sven:
//!    ```yaml
//!    channels:
//!      discord:
//!        bot_token: "${DISCORD_BOT_TOKEN}"
//!        guild_ids: []           # empty = all guilds
//!        allowed_channel_ids: [] # empty = all channels
//!    ```

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::channel::{Channel, InboundMessage, OutboundMessage, ReplyContext};

/// Discord channel adapter.
///
/// Connects to Discord using the Gateway API via websocket.
/// Uses Discord's REST API for sending messages.
pub struct DiscordChannel {
    #[allow(dead_code)]
    bot_token: String,
    guild_ids: Vec<u64>,
    allowed_channel_ids: Vec<u64>,
    last_message_ids: Arc<Mutex<std::collections::HashMap<String, String>>>,
    client: reqwest::Client,
}

impl DiscordChannel {
    /// Create a new Discord channel adapter.
    pub fn new(
        bot_token: impl Into<String>,
        guild_ids: Vec<u64>,
        allowed_channel_ids: Vec<u64>,
    ) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        let token = bot_token.into();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bot {token}"))
                .expect("valid token header"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("sven-agent/1.0"),
        );

        Self {
            bot_token: token,
            guild_ids,
            allowed_channel_ids,
            last_message_ids: Arc::new(Mutex::new(std::collections::HashMap::new())),
            client: reqwest::Client::builder()
                .default_headers(headers)
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Discord HTTP client"),
        }
    }

    #[allow(dead_code)]
    fn is_allowed_channel(&self, channel_id: u64) -> bool {
        self.allowed_channel_ids.is_empty() || self.allowed_channel_ids.contains(&channel_id)
    }

    #[allow(dead_code)]
    fn discord_api(&self, path: &str) -> String {
        format!("https://discord.com/api/v10{path}")
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        info!("Discord channel starting (REST polling)");

        let client = self.client.clone();
        let guild_ids = self.guild_ids.clone();
        let allowed_channel_ids = self.allowed_channel_ids.clone();
        let last_ids = self.last_message_ids.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                // Fetch guilds to poll
                let guilds_result = client
                    .get("https://discord.com/api/v10/users/@me/guilds".to_string())
                    .send()
                    .await;

                let guilds: Vec<Value> = match guilds_result {
                    Ok(r) => r.json().await.unwrap_or_default(),
                    Err(e) => {
                        error!(error = %e, "Discord: failed to fetch guilds");
                        continue;
                    }
                };

                for guild in &guilds {
                    let guild_id = match guild["id"].as_str() {
                        Some(id) => id.to_string(),
                        None => continue,
                    };

                    // Filter by configured guild_ids
                    if !guild_ids.is_empty() {
                        let id_u64: u64 = guild_id.parse().unwrap_or(0);
                        if !guild_ids.contains(&id_u64) {
                            continue;
                        }
                    }

                    // Get channels for this guild
                    let channels_result = client
                        .get(format!(
                            "https://discord.com/api/v10/guilds/{guild_id}/channels"
                        ))
                        .send()
                        .await;

                    let channels: Vec<Value> = match channels_result {
                        Ok(r) => r.json().await.unwrap_or_default(),
                        Err(e) => {
                            error!(error = %e, guild_id, "Discord: failed to fetch channels");
                            continue;
                        }
                    };

                    for ch in &channels {
                        // Only text channels (type 0)
                        if ch["type"].as_u64() != Some(0) {
                            continue;
                        }

                        let ch_id = match ch["id"].as_str() {
                            Some(id) => id.to_string(),
                            None => continue,
                        };

                        let ch_id_u64: u64 = ch_id.parse().unwrap_or(0);
                        if !allowed_channel_ids.is_empty()
                            && !allowed_channel_ids.contains(&ch_id_u64)
                        {
                            continue;
                        }

                        // Get the last seen message id
                        let after_id = {
                            let ids = last_ids.lock().await;
                            ids.get(&ch_id).cloned()
                        };

                        let mut req = client.get(format!(
                            "https://discord.com/api/v10/channels/{ch_id}/messages"
                        ));

                        if let Some(after) = &after_id {
                            req = req.query(&[("after", after.as_str()), ("limit", "10")]);
                        } else {
                            req = req.query(&[("limit", "1")]);
                        }

                        let messages: Vec<Value> = match req.send().await {
                            Ok(r) => r.json().await.unwrap_or_default(),
                            Err(e) => {
                                warn!(error = %e, ch_id, "Discord: message fetch error");
                                continue;
                            }
                        };

                        for message in messages.iter().rev() {
                            let msg_id = match message["id"].as_str() {
                                Some(id) => id.to_string(),
                                None => continue,
                            };

                            // Track highest seen id
                            {
                                let mut ids = last_ids.lock().await;
                                let current =
                                    ids.entry(ch_id.clone()).or_insert_with(|| msg_id.clone());
                                if msg_id > *current {
                                    *current = msg_id.clone();
                                }
                            }

                            // Skip bots
                            if message["author"]["bot"].as_bool() == Some(true) {
                                continue;
                            }

                            let text = message["content"].as_str().unwrap_or("").to_string();
                            if text.is_empty() {
                                continue;
                            }

                            let sender_id =
                                message["author"]["id"].as_str().unwrap_or("").to_string();
                            let sender_name = message["author"]["username"]
                                .as_str()
                                .map(|s| s.to_string());

                            let inbound = InboundMessage {
                                channel: "discord".to_string(),
                                sender: sender_id,
                                sender_name,
                                text,
                                attachments: vec![],
                                reply_context: ReplyContext {
                                    message_id: Some(msg_id),
                                    thread_id: Some(ch_id.clone()),
                                },
                            };

                            if tx.send(inbound).await.is_err() {
                                warn!("Discord: inbound channel closed — stopping");
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
        // recipient is the channel_id (thread_id from reply_context)
        let channel_id = msg
            .reply_context
            .as_ref()
            .and_then(|c| c.thread_id.as_deref())
            .unwrap_or(&msg.recipient);

        let payload = serde_json::json!({ "content": msg.text });

        self.client
            .post(format!(
                "https://discord.com/api/v10/channels/{channel_id}/messages"
            ))
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        info!("Discord channel stopping");
        Ok(())
    }
}
