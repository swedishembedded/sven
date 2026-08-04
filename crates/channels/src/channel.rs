// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Core channel trait and message types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A binary attachment (image, audio, document) carried with a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// MIME type (e.g. `"image/png"`, `"audio/ogg"`).
    pub mime_type: String,
    /// Raw bytes of the attachment.
    pub data: Vec<u8>,
    /// Optional filename hint.
    pub filename: Option<String>,
}

/// Context needed to reply to an inbound message on the same thread/topic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplyContext {
    /// Platform-specific message ID of the message being replied to.
    pub message_id: Option<String>,
    /// Platform-specific thread or conversation ID.
    pub thread_id: Option<String>,
}

/// An inbound message received from a messaging channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Channel identifier (e.g. `"telegram"`, `"discord"`).
    pub channel: String,
    /// Platform-specific sender identifier (user ID, username, phone number, etc.).
    pub sender: String,
    /// Human-readable sender display name, if available.
    pub sender_name: Option<String>,
    /// Text content of the message.
    pub text: String,
    /// Binary attachments (images, audio, documents).
    pub attachments: Vec<Attachment>,
    /// Context for replying inline.
    pub reply_context: ReplyContext,
}

/// An outbound message to send via a messaging channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Channel identifier (e.g. `"telegram"`, `"discord"`).
    pub channel: String,
    /// Platform-specific recipient identifier.
    pub recipient: String,
    /// Text content to send.
    pub text: String,
    /// Optional attachments.
    pub attachments: Vec<Attachment>,
    /// Optional reply context (thread the reply).
    pub reply_context: Option<ReplyContext>,
}

/// A messaging channel adapter.
///
/// Each channel implementation manages the lifecycle of one messaging platform
/// connection. Inbound messages are forwarded via the `mpsc::Sender` supplied
/// to [`Channel::start`]; outbound messages are sent via [`Channel::send`].
#[async_trait]
pub trait Channel: Send + Sync {
    /// Short identifier for this channel (e.g. `"telegram"`, `"discord"`).
    fn name(&self) -> &str;

    /// Start listening for inbound messages and forward them to `tx`.
    ///
    /// This call should return quickly after spawning background tasks.
    /// The background tasks continue running until [`Channel::stop`] is called.
    async fn start(&self, tx: tokio::sync::mpsc::Sender<InboundMessage>) -> anyhow::Result<()>;

    /// Send an outbound message.
    async fn send(&self, msg: OutboundMessage) -> anyhow::Result<()>;

    /// Stop all background tasks and release platform connections.
    async fn stop(&self) -> anyhow::Result<()>;
}
