// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Telegram Bot API bridge — connects inbound Telegram messages to the
//! node's `ControlService` and streams the agent's reply back.
//!
//! # How it works
//!
//! ```text
//! Telegram Bot API (long-poll getUpdates)
//!     │  InboundMessage (user_id, chat_id, text)
//!     ▼
//! TelegramBridge::run()  ──  spawns per-message task
//!     │
//!     ├─ sendChatAction("typing")   ──►  Telegram API
//!     │
//!     ├─ ControlCommand::NewSession  ──►  ControlService
//!     ├─ ControlCommand::SendInput   ──►  ControlService
//!     │
//!     │  ControlEvent::ToolCall / ToolResult / OutputComplete / SessionState
//!     │  (broadcast receiver filtered by session_id)
//!     │
//!     └─ sendMessage(formatted response)  ──►  Telegram API
//! ```
//!
//! # Per-user sessions
//!
//! Each allowed Telegram user ID gets a dedicated `ControlService` session UUID.
//! Sessions persist across messages (perpetual conversation). The `/clear` command
//! drops the session so the next message starts a fresh conversation.
//!
//! # Concurrency
//!
//! Multiple users are served concurrently — each user's message handling runs in
//! its own tokio task. The underlying `ControlService` serialises agent access
//! (the agent is not re-entrant), so users queue naturally. A per-user
//! `is_handling` flag prevents a second message from racing while the first is
//! still running; the user receives a friendly "busy" reply instead.
//!
//! # Typing indicator
//!
//! `sendChatAction("typing")` is sent immediately and refreshed every 4 seconds
//! while the agent processes the request. Telegram shows the "typing…" status for
//! 5 seconds per call, so refreshing at 4 s keeps it continuous.
//!
//! # Message length
//!
//! Telegram limits messages to 4096 bytes. Responses exceeding this limit are
//! split at paragraph boundaries and sent as multiple messages.
//!
//! # Tool call formatting
//!
//! When the agent invokes tools, a compact summary is prepended to the response:
//!
//! ```text
//! 🔧 `shell_command`(command=ls -la)
//! ```
//! exit 0
//! drwxr-xr-x ...
//! ```
//! ```

use std::{collections::HashMap, sync::Arc, time::Duration};

use sven_channels::channels::telegram::TelegramChannel;
use sven_channels::channel::{Channel, InboundMessage};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use sven_config::AgentMode;

use crate::control::{
    protocol::{ControlCommand, ControlEvent, SessionState},
    service::AgentHandle,
};

// ── Constants ─────────────────────────────────────────────────────────────────

const TELEGRAM_API_BASE: &str = "https://api.telegram.org/bot";

/// Telegram typing action expires after 5 s; refresh every 4 s to keep it alive.
const TYPING_REFRESH_SECS: u64 = 4;

/// Telegram message length limit (bytes).
const MAX_MESSAGE_BYTES: usize = 4096;

/// Maximum characters shown from a tool result in the summary.
const MAX_TOOL_OUTPUT_CHARS: usize = 300;

/// Maximum characters shown for a single tool argument value.
const MAX_ARG_VALUE_CHARS: usize = 50;

/// Timeout for a single agent turn (15 minutes).
const AGENT_TIMEOUT_SECS: u64 = 900;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Bookkeeping for a single Telegram user's ControlService session.
struct UserSession {
    /// The ControlService session UUID for this user.
    session_id: Uuid,
    /// True while an agent run is in progress for this user.
    /// Prevents a second concurrent request from racing with the first.
    is_handling: bool,
}

/// One tool invocation recorded during an agent turn.
struct ToolActivity {
    name: String,
    args: serde_json::Value,
    output: Option<String>,
    is_error: bool,
}

// ── Bridge ────────────────────────────────────────────────────────────────────

/// Bridges Telegram messages to the node's `ControlService`.
///
/// Create with [`TelegramBridge::new`], then call [`TelegramBridge::run`] which
/// blocks until the inbound channel closes.  Internally, it spawns one tokio
/// task per inbound message so multiple users are served concurrently.
pub struct TelegramBridge {
    bot_token: String,
    allowed_users: Vec<i64>,
    agent: AgentHandle,
    sessions: Arc<Mutex<HashMap<i64, UserSession>>>,
    client: reqwest::Client,
}

impl TelegramBridge {
    /// Create a new bridge.
    ///
    /// `bot_token` — Telegram bot token (must already be env-expanded).
    /// `allowed_users` — permitted Telegram user IDs; empty means allow all.
    /// `agent` — handle to the running `ControlService`.
    pub fn new(bot_token: String, allowed_users: Vec<i64>, agent: AgentHandle) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(35))
            .build()
            .expect("Telegram HTTP client");

        Self {
            bot_token,
            allowed_users,
            agent,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            client,
        }
    }

    /// Start the long-polling loop.  Returns when the inbound channel closes.
    pub async fn run(self) {
        let bridge = Arc::new(self);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<InboundMessage>(256);

        let channel =
            TelegramChannel::new(bridge.bot_token.clone(), bridge.allowed_users.clone());

        if let Err(e) = channel.start(tx).await {
            error!("Failed to start Telegram long-poll loop: {e}");
            return;
        }

        info!("Telegram bridge running — waiting for messages (long-polling)");

        while let Some(msg) = rx.recv().await {
            let user_id: i64 = msg.sender.parse().unwrap_or(0);
            let username = msg.sender_name.as_deref().unwrap_or("<unknown>");
            info!(
                user_id,
                username,
                text = %msg.text,
                "telegram: message received"
            );

            let b = Arc::clone(&bridge);
            tokio::spawn(async move {
                b.handle_message(msg).await;
            });
        }

        warn!("Telegram bridge: inbound channel closed");
    }

    // ── Message handling ─────────────────────────────────────────────────────

    async fn handle_message(self: &Arc<Self>, msg: InboundMessage) {
        let user_id: i64 = msg.sender.parse().unwrap_or(0);

        // Use the chat_id from ReplyContext (= Telegram chat ID) for replies.
        let chat_id = msg
            .reply_context
            .thread_id
            .clone()
            .unwrap_or_else(|| msg.sender.clone());

        // ── /clear command ────────────────────────────────────────────────────
        if msg.text.trim() == "/clear" {
            self.sessions.lock().await.remove(&user_id);
            info!(user_id, "telegram: conversation cleared by user");
            self.send_message(&chat_id, "Conversation cleared\\. Starting fresh\\.").await;
            return;
        }

        // ── Check per-user busy flag ──────────────────────────────────────────
        {
            let mut sessions = self.sessions.lock().await;
            if let Some(s) = sessions.get_mut(&user_id) {
                if s.is_handling {
                    drop(sessions);
                    info!(user_id, "telegram: user already being served — busy reply");
                    self.send_message(
                        &chat_id,
                        "I'm still processing your previous message\\. Please wait\\.",
                    )
                    .await;
                    return;
                }
                s.is_handling = true;
            }
            // If no session yet, we create it below; we'll set is_handling there.
        }

        // ── Get or create ControlService session ──────────────────────────────
        let session_id = {
            let mut sessions = self.sessions.lock().await;

            match sessions.get(&user_id) {
                Some(s) => s.session_id,
                None => {
                    let id = Uuid::new_v4();
                    if let Err(e) = self
                        .agent
                        .send(ControlCommand::NewSession {
                            id,
                            mode: AgentMode::Agent,
                            working_dir: None,
                        })
                        .await
                    {
                        error!(user_id, "telegram: failed to create session: {e}");
                        drop(sessions);
                        self.send_message(&chat_id, "Sorry, I could not start a session\\.")
                            .await;
                        return;
                    }
                    sessions.insert(
                        user_id,
                        UserSession {
                            session_id: id,
                            is_handling: true,
                        },
                    );
                    id
                }
            }
        };

        // ── Subscribe to events BEFORE sending input ──────────────────────────
        // Subscribing first prevents a race where the session completes before
        // we start listening.
        let mut events = self.agent.subscribe();

        // ── Send initial typing indicator ─────────────────────────────────────
        self.send_chat_action(&chat_id, "typing").await;

        // ── Submit user input to the agent ────────────────────────────────────
        if let Err(e) = self
            .agent
            .send(ControlCommand::SendInput {
                session_id,
                text: msg.text.clone(),
            })
            .await
        {
            error!(user_id, "telegram: failed to send input: {e}");
            self.clear_handling(user_id).await;
            self.send_message(&chat_id, "Sorry, I encountered an error sending your message\\.")
                .await;
            return;
        }

        // ── Collect events with periodic typing refresh ───────────────────────
        let mut typing_interval =
            tokio::time::interval(Duration::from_secs(TYPING_REFRESH_SECS));
        // The first tick fires immediately; skip it since we already sent above.
        typing_interval.tick().await;

        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(AGENT_TIMEOUT_SECS);

        let mut final_text = String::new();
        let mut tool_activities: Vec<ToolActivity> = Vec::new();

        // Maps call_id → (tool_name, args) for correlating ToolCall/ToolResult.
        let mut pending_tool_calls: HashMap<String, (String, serde_json::Value)> =
            HashMap::new();

        let mut done = false;

        while !done {
            tokio::select! {
                // Refresh typing indicator every TYPING_REFRESH_SECS.
                _ = typing_interval.tick() => {
                    self.send_chat_action(&chat_id, "typing").await;
                }

                // Agent events.
                result = events.recv() => {
                    match result {
                        Ok(ev) => {
                            done = self.process_event(
                                ev,
                                session_id,
                                user_id,
                                &mut final_text,
                                &mut tool_activities,
                                &mut pending_tool_calls,
                            ).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(user_id, "telegram: broadcast lagged {n} events — some events may be lost");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            warn!(user_id, "telegram: broadcast channel closed");
                            done = true;
                        }
                    }
                }

                // Hard timeout.
                _ = tokio::time::sleep_until(deadline) => {
                    warn!(user_id, "telegram: agent timed out after {AGENT_TIMEOUT_SECS}s");
                    final_text =
                        "I'm sorry — I timed out processing your message\\.".to_string();
                    done = true;
                }
            }
        }

        // ── Send the formatted response ───────────────────────────────────────
        let response = format_response(&tool_activities, &final_text);

        for chunk in split_message(&response) {
            self.send_message(&chat_id, &chunk).await;
        }

        // ── Release the per-user busy flag ────────────────────────────────────
        self.clear_handling(user_id).await;
    }

    /// Process one broadcast event. Returns `true` when the session is done.
    #[allow(clippy::too_many_arguments)]
    async fn process_event(
        &self,
        ev: ControlEvent,
        session_id: Uuid,
        user_id: i64,
        final_text: &mut String,
        tool_activities: &mut Vec<ToolActivity>,
        pending_tool_calls: &mut HashMap<String, (String, serde_json::Value)>,
    ) -> bool {
        match ev {
            // Accumulate the last complete assistant text.
            ControlEvent::OutputComplete {
                session_id: sid,
                text,
                role,
            } if sid == session_id && role == "assistant" => {
                *final_text = text;
            }

            // Record the start of a tool call.
            ControlEvent::ToolCall {
                session_id: sid,
                call_id,
                tool_name,
                args,
            } if sid == session_id => {
                info!(user_id, tool = %tool_name, "telegram: tool call started");
                pending_tool_calls.insert(call_id, (tool_name, args));
            }

            // Correlate the tool result with the pending call.
            ControlEvent::ToolResult {
                session_id: sid,
                call_id,
                output,
                is_error,
            } if sid == session_id => {
                if let Some((name, args)) = pending_tool_calls.remove(&call_id) {
                    debug!(user_id, tool = %name, is_error, "telegram: tool call finished");
                    tool_activities.push(ToolActivity {
                        name,
                        args,
                        output: Some(output),
                        is_error,
                    });
                }
            }

            // Session lifecycle.
            ControlEvent::SessionState {
                session_id: sid,
                state,
            } if sid == session_id => match state {
                SessionState::Completed | SessionState::Cancelled => {
                    return true;
                }
                _ => {}
            },

            // Agent-level error for our session.
            ControlEvent::AgentError {
                session_id: Some(sid),
                message,
            } if sid == session_id => {
                warn!(user_id, "telegram: agent error: {message}");
                *final_text = format!("I encountered an error: {}", escape_markdown_v2(&message));
                return true;
            }

            // Node-level error — may not be ours; log and continue.
            ControlEvent::NodeError { code, message } => {
                warn!(user_id, code, "telegram: node error: {message}");
                if code == 409 {
                    // Our SendInput raced with a still-running session.
                    // The busy-flag check above should normally prevent this,
                    // but handle defensively.
                    *final_text =
                        "I'm still processing your previous message\\. Please wait\\."
                            .to_string();
                    return true;
                }
                if code == 404 {
                    // Session expired on the ControlService side (e.g. node restart).
                    // Clear our stale session record so the next message creates fresh.
                    self.sessions.lock().await.remove(&user_id);
                    *final_text =
                        "Session expired\\. Please send your message again\\.".to_string();
                    return true;
                }
            }

            _ => {}
        }

        false
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Release the per-user `is_handling` flag.
    async fn clear_handling(&self, user_id: i64) {
        if let Some(s) = self.sessions.lock().await.get_mut(&user_id) {
            s.is_handling = false;
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}{}/{}", TELEGRAM_API_BASE, self.bot_token, method)
    }

    /// Send a Telegram message.  Tries MarkdownV2 first; falls back to plain
    /// text if the parse fails (e.g. unescaped special characters in LLM output).
    async fn send_message(&self, chat_id: &str, text: &str) {
        if !self
            .try_send_message(chat_id, text, Some("MarkdownV2"))
            .await
        {
            // MarkdownV2 failed — strip our escape sequences and send plain.
            let plain = unescape_markdown_v2(text);
            if !self.try_send_message(chat_id, &plain, None).await {
                warn!(chat_id, "telegram: sendMessage failed even in plain-text mode");
            }
        }
    }

    /// Attempt to send a message with an optional parse_mode.  Returns true on success.
    async fn try_send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
    ) -> bool {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });

        if let Some(mode) = parse_mode {
            payload["parse_mode"] = serde_json::Value::String(mode.to_string());
        }

        match self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    true
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    debug!(
                        chat_id,
                        parse_mode,
                        status = %status,
                        body = %body,
                        "telegram: sendMessage non-success"
                    );
                    false
                }
            }
            Err(e) => {
                error!(chat_id, "telegram: sendMessage network error: {e}");
                false
            }
        }
    }

    /// Send a `sendChatAction` (e.g. "typing").  Failures are logged at debug
    /// level only — they are ephemeral and not user-visible.
    async fn send_chat_action(&self, chat_id: &str, action: &str) {
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "action": action,
        });

        match self
            .client
            .post(self.api_url("sendChatAction"))
            .json(&payload)
            .send()
            .await
        {
            Ok(_) => {}
            Err(e) => {
                debug!(chat_id, action, "telegram: sendChatAction error: {e}");
            }
        }
    }
}

// ── Message formatting ────────────────────────────────────────────────────────

/// Build the full Telegram message from tool activities and the final LLM text.
///
/// Layout:
/// ```text
/// 🔧 `tool_name`(arg=value, …)
/// ```
/// output snippet
/// ```
///
/// [final LLM response]
/// ```
fn format_response(activities: &[ToolActivity], final_text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();

    if !activities.is_empty() {
        let mut tool_section = String::new();
        for act in activities {
            // Tool header line.
            let args_summary = format_args_summary(&act.args);
            let tool_line = format!(
                "🔧 `{}`{}",
                escape_markdown_v2(&act.name),
                args_summary
            );
            tool_section.push_str(&tool_line);
            tool_section.push('\n');

            // Optional output snippet.
            if let Some(ref output) = act.output {
                let output_trimmed = output.trim();
                if !output_trimmed.is_empty() {
                    let snippet = truncate_chars(output_trimmed, MAX_TOOL_OUTPUT_CHARS);
                    let prefix = if act.is_error { "⚠️ " } else { "" };
                    tool_section.push_str(&format!(
                        "{}```\n{}\n```\n",
                        prefix,
                        snippet
                    ));
                }
            }
        }
        parts.push(tool_section.trim_end().to_string());
    }

    if !final_text.is_empty() {
        parts.push(final_text.to_string());
    }

    parts.join("\n\n")
}

/// Format tool arguments as a compact inline summary `(key=value, …)`.
/// Only the first few args are shown to keep the line short.
fn format_args_summary(args: &serde_json::Value) -> String {
    let map = match args.as_object() {
        Some(m) if !m.is_empty() => m,
        _ => return String::new(),
    };

    let items: Vec<String> = map
        .iter()
        .take(3)
        .map(|(k, v)| {
            let val = match v {
                serde_json::Value::String(s) => truncate_chars(s, MAX_ARG_VALUE_CHARS).to_string(),
                serde_json::Value::Null => "null".to_string(),
                _ => truncate_chars(&v.to_string(), MAX_ARG_VALUE_CHARS).to_string(),
            };
            format!("{}\\={}", escape_markdown_v2(k), escape_markdown_v2(&val))
        })
        .collect();

    if items.is_empty() {
        String::new()
    } else {
        format!("\\({}\\)", items.join(", "))
    }
}

/// Split a message into chunks of at most `MAX_MESSAGE_BYTES` bytes.
///
/// Splits at double-newline (paragraph) boundaries first, then single newlines,
/// then at the byte limit as a last resort.  Ensures splits never bisect a
/// UTF-8 multi-byte sequence.
fn split_message(text: &str) -> Vec<String> {
    if text.len() <= MAX_MESSAGE_BYTES {
        return vec![text.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut remaining = text;

    while remaining.len() > MAX_MESSAGE_BYTES {
        // Find a UTF-8-safe split boundary at or before the limit.
        let mut limit = MAX_MESSAGE_BYTES;
        while !remaining.is_char_boundary(limit) {
            limit -= 1;
        }

        // Prefer paragraph break, then line break, then hard cut.
        let split_at = remaining[..limit]
            .rfind("\n\n")
            .map(|p| p + 2)
            .or_else(|| remaining[..limit].rfind('\n').map(|p| p + 1))
            .unwrap_or(limit);

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }

    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }

    chunks
}

// ── MarkdownV2 helpers ────────────────────────────────────────────────────────

/// Escape all characters that have special meaning in Telegram MarkdownV2.
///
/// Required characters: `_ * [ ] ( ) ~ ` > # + - = | { } . !`
pub fn escape_markdown_v2(s: &str) -> String {
    const SPECIAL: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        if SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Remove backslash-escapes added by [`escape_markdown_v2`] to produce plain text.
fn unescape_markdown_v2(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if next != '\\' {
                    // Skip the backslash — the next char is the literal.
                    chars.next();
                    out.push(next);
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}

/// Return a string slice (or owned String) truncated to at most `max_chars`
/// Unicode scalar values.  Appends `…` when truncated.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut iter = s.char_indices();
    let mut end = s.len();
    let mut count = 0;

    for (i, _) in s.char_indices() {
        if count >= max_chars {
            end = i;
            break;
        }
        count += 1;
        let _ = iter.next();
    }

    if end == s.len() {
        s.to_string()
    } else {
        format!("{}…", &s[..end])
    }
}

// ── Environment variable expansion ───────────────────────────────────────────

/// Expand `${VAR_NAME}` placeholders in `s` with the values of the named
/// environment variables.  Unknown variables are replaced with an empty string.
pub fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    loop {
        let start = match result.find("${") {
            Some(i) => i,
            None => break,
        };
        let after_brace = start + 2;
        let end = match result[after_brace..].find('}') {
            Some(i) => after_brace + i,
            None => break, // Unclosed `${` — stop.
        };
        let var_name = &result[after_brace..end].to_string();
        let value = std::env::var(var_name).unwrap_or_default();
        result = format!("{}{}{}", &result[..start], value, &result[end + 1..]);
    }
    result
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── escape_markdown_v2 ────────────────────────────────────────────────────

    #[test]
    fn escape_plain_ascii_unchanged() {
        assert_eq!(escape_markdown_v2("hello world"), "hello world");
    }

    #[test]
    fn escape_special_chars() {
        let s = "hello_world.test!";
        let escaped = escape_markdown_v2(s);
        assert!(escaped.contains("\\_"));
        assert!(escaped.contains("\\."));
        assert!(escaped.contains("\\!"));
    }

    #[test]
    fn unescape_roundtrip() {
        let original = "hello_world.test!";
        let escaped = escape_markdown_v2(original);
        let unescaped = unescape_markdown_v2(&escaped);
        assert_eq!(unescaped, original);
    }

    // ── truncate_chars ────────────────────────────────────────────────────────

    #[test]
    fn truncate_chars_short_string_unchanged() {
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn truncate_chars_long_string_gets_ellipsis() {
        let result = truncate_chars("hello world", 5);
        assert!(result.starts_with("hello"));
        assert!(result.ends_with('…'));
    }

    #[test]
    fn truncate_chars_unicode_safe() {
        let s = "héllo wörld";
        let result = truncate_chars(s, 5);
        assert!(result.ends_with('…'));
        // Must be valid UTF-8 — this would panic if we sliced at a byte boundary.
        let _: String = result.chars().collect();
    }

    // ── split_message ─────────────────────────────────────────────────────────

    #[test]
    fn split_message_short_text_single_chunk() {
        let chunks = split_message("hello");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello");
    }

    #[test]
    fn split_message_exact_limit_single_chunk() {
        let text = "a".repeat(MAX_MESSAGE_BYTES);
        let chunks = split_message(&text);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn split_message_over_limit_multiple_chunks() {
        let text = "a".repeat(MAX_MESSAGE_BYTES + 100);
        let chunks = split_message(&text);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= MAX_MESSAGE_BYTES);
        }
        // Reassembly must recover the original text.
        assert_eq!(chunks.join(""), text);
    }

    #[test]
    fn split_message_prefers_paragraph_boundary() {
        let para = "x".repeat(3000);
        // Two paragraphs separated by double newline, total > 4096.
        let text = format!("{}\n\n{}", para, para);
        let chunks = split_message(&text);
        // Should split between the two paragraphs.
        assert!(chunks.len() >= 2);
        assert!(!chunks[0].contains('\n') || chunks[0].ends_with('\n'));
    }

    #[test]
    fn split_message_all_chunks_valid_utf8() {
        // Build a string with multi-byte Unicode that exceeds the limit.
        let segment = "héllo wörld — tëst\n\n";
        let text = segment.repeat(300); // well over 4096 bytes
        let chunks = split_message(&text);
        for chunk in &chunks {
            // Panic here would indicate a broken UTF-8 slice.
            let _v: Vec<char> = chunk.chars().collect();
            assert!(chunk.len() <= MAX_MESSAGE_BYTES);
        }
    }

    // ── format_response ───────────────────────────────────────────────────────

    #[test]
    fn format_response_no_tools_returns_text_only() {
        let result = format_response(&[], "Hello from agent");
        assert_eq!(result, "Hello from agent");
    }

    #[test]
    fn format_response_tool_call_included() {
        let activities = vec![ToolActivity {
            name: "shell_command".to_string(),
            args: serde_json::json!({"command": "ls -la"}),
            output: Some("total 8\ndrwxr-xr-x".to_string()),
            is_error: false,
        }];
        let result = format_response(&activities, "Done.");
        assert!(result.contains("shell_command"));
        assert!(result.contains("ls"));
        assert!(result.contains("total 8"));
        assert!(result.contains("Done."));
    }

    #[test]
    fn format_response_error_tool_shows_warning() {
        let activities = vec![ToolActivity {
            name: "run_cmd".to_string(),
            args: serde_json::json!({}),
            output: Some("command not found".to_string()),
            is_error: true,
        }];
        let result = format_response(&activities, "");
        assert!(result.contains("⚠️"));
        assert!(result.contains("command not found"));
    }

    #[test]
    fn format_response_empty_final_text_returns_tools_only() {
        let activities = vec![ToolActivity {
            name: "ls".to_string(),
            args: serde_json::json!({}),
            output: None,
            is_error: false,
        }];
        let result = format_response(&activities, "");
        assert!(result.contains("ls"));
        assert!(!result.is_empty());
    }

    // ── expand_env_vars ───────────────────────────────────────────────────────

    #[test]
    fn expand_env_vars_no_placeholders() {
        assert_eq!(expand_env_vars("plain token"), "plain token");
    }

    #[test]
    fn expand_env_vars_known_var() {
        std::env::set_var("SVEN_TEST_VAR", "abc123");
        let result = expand_env_vars("${SVEN_TEST_VAR}");
        assert_eq!(result, "abc123");
    }

    #[test]
    fn expand_env_vars_unknown_var_becomes_empty() {
        let result = expand_env_vars("${_NO_SUCH_VAR_XYZ_}");
        assert_eq!(result, "");
    }

    #[test]
    fn expand_env_vars_multiple_placeholders() {
        std::env::set_var("SVEN_A", "foo");
        std::env::set_var("SVEN_B", "bar");
        let result = expand_env_vars("${SVEN_A}-${SVEN_B}");
        assert_eq!(result, "foo-bar");
    }

    #[test]
    fn expand_env_vars_unclosed_brace_unchanged() {
        let s = "token_${INCOMPLETE";
        assert_eq!(expand_env_vars(s), s);
    }
}
