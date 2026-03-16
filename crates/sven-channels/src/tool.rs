// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `send_message` tool — lets the agent proactively send messages to any
//! configured messaging channel.

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
};

use crate::channel::OutboundMessage;
use crate::manager::ChannelManager;

/// Tool that sends a message via any configured messaging channel.
///
/// The agent uses this tool to proactively deliver briefings, alerts,
/// summaries, and replies to the user via their preferred messaging platform.
///
/// # Example tool call
/// ```json
/// {
///   "channel": "telegram",
///   "recipient": "123456789",
///   "text": "Your daily briefing: ...",
/// }
/// ```
pub struct SendMessageTool {
    manager: ChannelManager,
}

impl SendMessageTool {
    pub fn new(manager: ChannelManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to a user via a configured messaging channel \
         (telegram, discord, whatsapp, signal, matrix, irc, slack).\n\
         Use this to deliver proactive notifications, daily briefings, \
         alerts, and responses outside the current conversation."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel name: telegram | discord | whatsapp | signal | matrix | irc | slack"
                },
                "recipient": {
                    "type": "string",
                    "description": "Platform-specific recipient ID (chat ID, user ID, phone number, channel name, etc.)"
                },
                "text": {
                    "type": "string",
                    "description": "Text content to send. Markdown is supported on most platforms."
                }
            },
            "required": ["channel", "recipient", "text"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let channel = match call.args.get("channel").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'channel'"),
        };
        let recipient = match call.args.get("recipient").and_then(|v| v.as_str()) {
            Some(r) => r.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'recipient'"),
        };
        let text = match call.args.get("text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'text'"),
        };

        let msg = OutboundMessage {
            channel: channel.clone(),
            recipient,
            text,
            attachments: vec![],
            reply_context: None,
        };

        match self.manager.send(msg).await {
            Ok(()) => ToolOutput::ok(&call.id, format!("Message sent via {channel}.")),
            Err(e) => ToolOutput::err(&call.id, format!("send_message failed: {e}")),
        }
    }
}

impl ToolDisplay for SendMessageTool {
    fn display_name(&self) -> &str {
        "SendMessage"
    }
    fn icon(&self) -> &str {
        "💬"
    }
    fn category(&self) -> &str {
        "messaging"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let channel = args.get("channel").and_then(|v| v.as_str()).unwrap_or("?");
        let recipient = args
            .get("recipient")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        format!("→ {channel}/{recipient}")
    }
}
