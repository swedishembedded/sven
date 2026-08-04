// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Messaging channel integrations for sven.
//!
//! This crate provides the [`Channel`] trait and a [`ChannelManager`] that
//! drives multiple messaging platforms from a single interface. The agent
//! receives inbound messages and sends outbound replies through the same
//! abstraction regardless of the underlying platform.
//!
//! # Supported channels
//!
//! | Channel | Feature flag | Requirements |
//! |---------|-------------|--------------|
//! | Telegram | `telegram` | `TELEGRAM_BOT_TOKEN` |
//! | Discord  | `discord`  | `DISCORD_BOT_TOKEN` |
//! | WhatsApp | (built-in HTTP) | Meta Business account |
//! | Signal   | (built-in stdio) | `signal-cli` binary |
//! | Matrix   | (built-in HTTP) | homeserver credentials |
//! | IRC      | (built-in TCP) | server credentials |
//!
//! # Quick start
//!
//! ```no_run
//! use sven_channels::{ChannelManager, InboundMessage};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let (tx, mut rx) = tokio::sync::mpsc::channel::<InboundMessage>(64);
//! let manager = ChannelManager::new();
//! // manager.add_channel(Box::new(telegram_channel));
//! manager.start(tx).await?;
//!
//! while let Some(msg) = rx.recv().await {
//!     println!("{}: {}", msg.sender, msg.text);
//!     manager.send(sven_channels::OutboundMessage {
//!         channel: msg.channel.clone(),
//!         recipient: msg.sender.clone(),
//!         text: "I received your message!".into(),
//!         attachments: vec![],
//!         reply_context: None,
//!     }).await?;
//! }
//! # Ok(())
//! # }
//! ```

pub mod channel;
pub mod channels;
pub mod manager;
pub mod tool;

pub use channel::{Attachment, Channel, InboundMessage, OutboundMessage, ReplyContext};
pub use manager::ChannelManager;
pub use tool::SendMessageTool;
