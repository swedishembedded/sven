// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `email` tool — agent access to email via any configured provider.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
};

use super::{EmailProvider, EmailQuery, NewEmail};

/// Tool providing the agent with email access.
///
/// # Actions
///
/// - `list` — list recent messages (optional folder, unread_only, limit)
/// - `read` — read the full body of a message by ID
/// - `send` — send a new email (to, subject, body)
/// - `reply` — reply to a message by ID
/// - `search` — search messages by keyword
pub struct EmailTool {
    provider: Arc<dyn EmailProvider>,
}

impl EmailTool {
    pub fn new(provider: Arc<dyn EmailProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for EmailTool {
    fn name(&self) -> &str {
        "email"
    }

    fn description(&self) -> &str {
        "Read and send email via the configured email provider.\n\
         Actions: list | read | send | reply | search\n\
         Use list/search to check inbox for important messages, read for details, \
         send/reply to respond on behalf of the user."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "read", "send", "reply", "search"],
                    "description": "Email operation to perform"
                },
                "folder": {
                    "type": "string",
                    "description": "(list) Mailbox folder. Default: INBOX"
                },
                "limit": {
                    "type": "integer",
                    "description": "(list) Maximum messages to return. Default: 20"
                },
                "unread_only": {
                    "type": "boolean",
                    "description": "(list) Only return unread messages. Default: false"
                },
                "from_filter": {
                    "type": "string",
                    "description": "(list) Filter by sender address substring"
                },
                "subject_filter": {
                    "type": "string",
                    "description": "(list) Filter by subject substring"
                },
                "id": {
                    "type": "string",
                    "description": "(read/reply) Message ID returned by list or search"
                },
                "to": {
                    "type": "string",
                    "description": "(send) Recipient email address"
                },
                "subject": {
                    "type": "string",
                    "description": "(send) Email subject line"
                },
                "body": {
                    "type": "string",
                    "description": "(send/reply) Plain text message body"
                },
                "query": {
                    "type": "string",
                    "description": "(search) Search keywords"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolOutput::err(&call.id, "missing 'action'"),
        };

        match action {
            "list" => self.list(call).await,
            "read" => self.read(call).await,
            "send" => self.send(call).await,
            "reply" => self.reply(call).await,
            "search" => self.search(call).await,
            other => ToolOutput::err(
                &call.id,
                format!("unknown action {other:?}; expected list|read|send|reply|search"),
            ),
        }
    }
}

impl EmailTool {
    async fn list(&self, call: &ToolCall) -> ToolOutput {
        let query = EmailQuery {
            folder: call
                .args
                .get("folder")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            limit: call
                .args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize),
            unread_only: call
                .args
                .get("unread_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            from: call
                .args
                .get("from_filter")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            subject: call
                .args
                .get("subject_filter")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            since: None,
        };

        match self.provider.list(&query).await {
            Ok(summaries) => {
                if summaries.is_empty() {
                    return ToolOutput::ok(&call.id, "No messages found.");
                }
                let lines: Vec<String> = summaries
                    .iter()
                    .map(|s| {
                        let unread = if s.unread { "[UNREAD] " } else { "" };
                        format!(
                            "- {unread}ID: {} | From: {} | Subject: {}",
                            s.id, s.from, s.subject
                        )
                    })
                    .collect();
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
            Err(e) => ToolOutput::err(&call.id, format!("email list failed: {e}")),
        }
    }

    async fn read(&self, call: &ToolCall) -> ToolOutput {
        let id = match call.args.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return ToolOutput::err(&call.id, "read requires 'id'"),
        };

        match self.provider.read(&id).await {
            Ok(msg) => {
                let output = format!(
                    "From: {}\nTo: {}\nSubject: {}\n\n{}",
                    msg.from,
                    msg.to.join(", "),
                    msg.subject,
                    msg.body_text
                );
                ToolOutput::ok(&call.id, output)
            }
            Err(e) => ToolOutput::err(&call.id, format!("email read failed: {e}")),
        }
    }

    async fn send(&self, call: &ToolCall) -> ToolOutput {
        let to = match call.args.get("to").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolOutput::err(&call.id, "send requires 'to'"),
        };
        let subject = match call.args.get("subject").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::err(&call.id, "send requires 'subject'"),
        };
        let body = match call.args.get("body").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return ToolOutput::err(&call.id, "send requires 'body'"),
        };

        let email = NewEmail::simple(to, subject, body);

        match self.provider.send(&email).await {
            Ok(()) => ToolOutput::ok(&call.id, "Email sent."),
            Err(e) => ToolOutput::err(&call.id, format!("email send failed: {e}")),
        }
    }

    async fn reply(&self, call: &ToolCall) -> ToolOutput {
        let id = match call.args.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return ToolOutput::err(&call.id, "reply requires 'id'"),
        };
        let body = match call.args.get("body").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return ToolOutput::err(&call.id, "reply requires 'body'"),
        };

        match self.provider.reply(&id, &body).await {
            Ok(()) => ToolOutput::ok(&call.id, "Reply sent."),
            Err(e) => ToolOutput::err(&call.id, format!("email reply failed: {e}")),
        }
    }

    async fn search(&self, call: &ToolCall) -> ToolOutput {
        let query = match call.args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return ToolOutput::err(&call.id, "search requires 'query'"),
        };

        match self.provider.search(&query).await {
            Ok(results) => {
                if results.is_empty() {
                    return ToolOutput::ok(&call.id, "No messages found.");
                }
                let lines: Vec<String> = results
                    .iter()
                    .map(|s| format!("- ID: {} | From: {} | Subject: {}", s.id, s.from, s.subject))
                    .collect();
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
            Err(e) => ToolOutput::err(&call.id, format!("email search failed: {e}")),
        }
    }
}

impl ToolDisplay for EmailTool {
    fn display_name(&self) -> &str {
        "Email"
    }
    fn icon(&self) -> &str {
        "📧"
    }
    fn category(&self) -> &str {
        "integrations"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        match action {
            "send" => {
                let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                format!("send → {to}")
            }
            "reply" => {
                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                format!("reply to {id}")
            }
            "search" => {
                let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("?");
                format!("search '{q}'")
            }
            _ => action.to_string(),
        }
    }
}
