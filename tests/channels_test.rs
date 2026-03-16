// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Integration tests for the channel → manager → reply flow.
//!
//! These tests exercise `ChannelManager` using an in-memory mock channel
//! to verify that inbound messages are received and outbound replies are
//! dispatched correctly without any external network calls.

use async_trait::async_trait;
use std::sync::Arc;
use sven_channels::{
    channel::{Channel, InboundMessage, OutboundMessage, ReplyContext},
    ChannelManager,
};
use tokio::sync::{mpsc, Mutex};

// ── Mock channel ──────────────────────────────────────────────────────────────

/// A fully in-memory channel: `inject()` pushes messages to the listener;
/// `sent()` returns messages dispatched via `send()`.
struct MockChannel {
    id: &'static str,
    inject_rx: Arc<Mutex<Option<mpsc::Receiver<InboundMessage>>>>,
    sent: Arc<Mutex<Vec<OutboundMessage>>>,
}

impl MockChannel {
    fn new(
        id: &'static str,
    ) -> (
        Self,
        mpsc::Sender<InboundMessage>,
        Arc<Mutex<Vec<OutboundMessage>>>,
    ) {
        let (tx, rx) = mpsc::channel(64);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let ch = Self {
            id,
            inject_rx: Arc::new(Mutex::new(Some(rx))),
            sent: sent.clone(),
        };
        (ch, tx, sent)
    }
}

#[async_trait]
impl Channel for MockChannel {
    fn name(&self) -> &str {
        self.id
    }

    async fn start(&self, tx: mpsc::Sender<InboundMessage>) -> anyhow::Result<()> {
        let rx = self.inject_rx.lock().await.take();
        if let Some(mut rx) = rx {
            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    let _ = tx.send(msg).await;
                }
            });
        }
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn inbound_message_forwarded_through_manager() {
    let (mock_ch, inject_tx, _sent) = MockChannel::new("telegram");
    let mgr = ChannelManager::new();
    mgr.add_channel(Box::new(mock_ch)).await;

    let (bus_tx, mut bus_rx) = mpsc::channel::<InboundMessage>(16);
    mgr.start(bus_tx).await.unwrap();

    inject_tx
        .send(InboundMessage {
            channel: "telegram".to_string(),
            sender: "user123".to_string(),
            sender_name: Some("Alice".to_string()),
            text: "ping".to_string(),
            attachments: vec![],
            reply_context: ReplyContext::default(),
        })
        .await
        .unwrap();

    // Give the spawned task a moment to forward the message.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let msg = bus_rx.try_recv().expect("should have received the message");
    assert_eq!(msg.channel, "telegram");
    assert_eq!(msg.sender, "user123");
    assert_eq!(msg.text, "ping");
}

#[tokio::test]
async fn reply_dispatched_to_correct_channel() {
    let (mock_ch, _inject_tx, sent) = MockChannel::new("discord");
    let mgr = ChannelManager::new();
    mgr.add_channel(Box::new(mock_ch)).await;

    mgr.send(OutboundMessage {
        channel: "discord".to_string(),
        recipient: "channel-general".to_string(),
        text: "Hello from agent!".to_string(),
        attachments: vec![],
        reply_context: None,
    })
    .await
    .unwrap();

    let msgs = sent.lock().await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].text, "Hello from agent!");
    assert_eq!(msgs[0].recipient, "channel-general");
}

#[tokio::test]
async fn full_echo_flow() {
    // Simulate: inbound message → "agent" processes → reply to same channel.
    let (mock_ch, inject_tx, sent) = MockChannel::new("matrix");
    let mgr = ChannelManager::new();
    mgr.add_channel(Box::new(mock_ch)).await;

    let (bus_tx, mut bus_rx) = mpsc::channel::<InboundMessage>(16);
    mgr.start(bus_tx).await.unwrap();

    inject_tx
        .send(InboundMessage {
            channel: "matrix".to_string(),
            sender: "@alice:matrix.org".to_string(),
            sender_name: None,
            text: "what time is it?".to_string(),
            attachments: vec![],
            reply_context: ReplyContext::default(),
        })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let inbound = bus_rx.try_recv().expect("inbound message");

    // Simulate agent producing a reply and dispatching it.
    mgr.send(OutboundMessage {
        channel: inbound.channel.clone(),
        recipient: inbound.sender.clone(),
        text: "It is 09:00 UTC.".to_string(),
        attachments: vec![],
        reply_context: None,
    })
    .await
    .unwrap();

    let msgs = sent.lock().await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].recipient, "@alice:matrix.org");
    assert!(msgs[0].text.contains("09:00"));
}

#[tokio::test]
async fn send_to_missing_channel_errors_with_name_hint() {
    let mgr = ChannelManager::new();
    let (ch, _, _) = MockChannel::new("telegram");
    mgr.add_channel(Box::new(ch)).await;

    let err = mgr
        .send(OutboundMessage {
            channel: "whatsapp".to_string(),
            recipient: "+1234".to_string(),
            text: "hi".to_string(),
            attachments: vec![],
            reply_context: None,
        })
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("whatsapp"),
        "error should mention the unknown channel name"
    );
    assert!(
        msg.contains("telegram"),
        "error should list available channels"
    );
}
