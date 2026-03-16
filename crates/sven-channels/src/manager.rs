// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Channel manager — owns and drives all active channel instances.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::channel::{Channel, InboundMessage, OutboundMessage};

/// Manages all active messaging channel adapters.
///
/// `ChannelManager` is the single entry point for both receiving messages from
/// any channel and sending messages out to any channel. It is `Clone` and
/// `Send + Sync` so it can be shared across tasks (e.g. given to the
/// `SendMessageTool`).
#[derive(Clone)]
pub struct ChannelManager {
    channels: Arc<Mutex<Vec<Box<dyn Channel>>>>,
}

impl ChannelManager {
    /// Create an empty manager. Use [`add_channel`] to register adapters.
    pub fn new() -> Self {
        Self {
            channels: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a channel adapter.
    pub async fn add_channel(&self, channel: Box<dyn Channel>) {
        self.channels.lock().await.push(channel);
    }

    /// Start all registered channels and forward inbound messages to `tx`.
    ///
    /// Each channel spawns its own background task; this method returns as
    /// soon as all channels have been started.
    pub async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        let channels = self.channels.lock().await;
        for channel in channels.iter() {
            info!(channel = %channel.name(), "starting channel");
            if let Err(e) = channel.start(tx.clone()).await {
                error!(channel = %channel.name(), error = %e, "failed to start channel");
            }
        }
        Ok(())
    }

    /// Send an outbound message to the appropriate channel.
    ///
    /// The `msg.channel` field selects the destination adapter by name.
    /// Returns an error if no matching channel is registered.
    pub async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()> {
        let channels = self.channels.lock().await;
        for channel in channels.iter() {
            if channel.name() == msg.channel {
                return channel.send(msg).await;
            }
        }
        anyhow::bail!(
            "no channel named {:?} is registered; \
             available: [{}]",
            msg.channel,
            channels
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// Stop all channels gracefully.
    pub async fn stop(&self) {
        let channels = self.channels.lock().await;
        for channel in channels.iter() {
            if let Err(e) = channel.stop().await {
                warn!(channel = %channel.name(), error = %e, "error stopping channel");
            }
        }
    }

    /// Return the names of all registered channels.
    pub async fn channel_names(&self) -> Vec<String> {
        self.channels
            .lock()
            .await
            .iter()
            .map(|c| c.name().to_string())
            .collect()
    }
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// A minimal in-memory channel for testing.
    struct MockChannel {
        name: &'static str,
        sent: Arc<Mutex<Vec<OutboundMessage>>>,
    }

    impl MockChannel {
        fn new(name: &'static str) -> (Self, Arc<Mutex<Vec<OutboundMessage>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    name,
                    sent: sent.clone(),
                },
                sent,
            )
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            self.name
        }

        async fn start(&self, _tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()> {
            self.sent.lock().await.push(msg);
            Ok(())
        }

        async fn stop(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn channel_names_reflects_registered_channels() {
        let mgr = ChannelManager::new();
        let (ch1, _) = MockChannel::new("telegram");
        let (ch2, _) = MockChannel::new("discord");
        mgr.add_channel(Box::new(ch1)).await;
        mgr.add_channel(Box::new(ch2)).await;

        let names = mgr.channel_names().await;
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"telegram".to_string()));
        assert!(names.contains(&"discord".to_string()));
    }

    #[tokio::test]
    async fn send_routes_to_correct_channel() {
        let mgr = ChannelManager::new();
        let (ch, sent) = MockChannel::new("telegram");
        mgr.add_channel(Box::new(ch)).await;

        mgr.send(OutboundMessage {
            channel: "telegram".to_string(),
            recipient: "123".to_string(),
            text: "hello".to_string(),
            attachments: vec![],
            reply_context: None,
        })
        .await
        .unwrap();

        let msgs = sent.lock().await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "hello");
        assert_eq!(msgs[0].recipient, "123");
    }

    #[tokio::test]
    async fn send_to_unknown_channel_returns_error() {
        let mgr = ChannelManager::new();
        let result = mgr
            .send(OutboundMessage {
                channel: "nonexistent".to_string(),
                recipient: "x".to_string(),
                text: "test".to_string(),
                attachments: vec![],
                reply_context: None,
            })
            .await;

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("nonexistent"));
    }

    #[tokio::test]
    async fn start_calls_all_channels() {
        let mgr = ChannelManager::new();
        let (ch1, _) = MockChannel::new("alpha");
        let (ch2, _) = MockChannel::new("beta");
        mgr.add_channel(Box::new(ch1)).await;
        mgr.add_channel(Box::new(ch2)).await;

        let (tx, _rx) = mpsc::channel(16);
        mgr.start(tx).await.unwrap();
    }

    #[tokio::test]
    async fn empty_manager_send_errors() {
        let mgr = ChannelManager::new();
        assert!(mgr
            .send(OutboundMessage {
                channel: "any".to_string(),
                recipient: "x".to_string(),
                text: "t".to_string(),
                attachments: vec![],
                reply_context: None,
            })
            .await
            .is_err());
    }
}
