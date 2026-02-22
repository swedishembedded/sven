// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
/// YAML-configured mock model provider for end-to-end and bats tests.
///
/// The provider reads a YAML file that maps input patterns to canned
/// responses (text only, or a tool-call sequence followed by a final
/// text reply).  This makes test behaviour fully deterministic and lets
/// test authors express scenarios in readable YAML rather than Rust code.
///
/// # YAML format
///
/// ```yaml
/// responses:
///   - match_type: contains       # contains | equals | starts_with | regex | default
///     pattern: "ping"
///     reply: "pong"
///
///   - match_type: contains
///     pattern: "write a file"
///     tool_calls:
///       - id: tc-1
///         tool: fs
///         args:
///           operation: write
///           path: /tmp/test.txt
///           content: "hello"
///     after_tool_reply: "File written."
///
///   - match_type: default
///     reply: "I understand your request."
/// ```
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use async_trait::async_trait;
use futures::stream;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{provider::ResponseStream, CompletionRequest, ResponseEvent, Role};

// ─── YAML schema ─────────────────────────────────────────────────────────────

/// Root document.
#[derive(Debug, Deserialize)]
pub struct MockConfig {
    pub responses: Vec<ResponseRule>,
}

/// One entry in the responses list.
#[derive(Debug, Deserialize)]
pub struct ResponseRule {
    /// How to match the last user message.
    pub match_type: MatchType,
    /// Pattern string (ignored for `default` match type).
    #[serde(default)]
    pub pattern: String,
    /// Simple text reply (used when there are no tool_calls, or as the
    /// after-tool reply when tool_calls is also set).
    pub reply: Option<String>,
    /// Tool calls to emit in the first round.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDef>,
    /// Text reply to send after tool results arrive (second round).
    pub after_tool_reply: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchType {
    Contains,
    Equals,
    StartsWith,
    Regex,
    Default,
}

/// A single tool call defined in the YAML.
#[derive(Debug, Deserialize)]
pub struct ToolCallDef {
    pub id: String,
    pub tool: String,
    /// YAML map that is serialized to a JSON string for the tool arguments.
    pub args: serde_json::Value,
}

// ─── Provider ────────────────────────────────────────────────────────────────

/// A model provider whose responses are driven by a YAML configuration file.
/// Suitable for end-to-end tests and bats scripts.
pub struct YamlMockProvider {
    config: Arc<MockConfig>,
    /// Counts how many times `complete` has been called overall, used for
    /// debugging and deterministic sequencing.
    call_count: Arc<Mutex<u32>>,
    name: String,
}

impl YamlMockProvider {
    /// Load a provider from a YAML file at `path`.
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading mock responses file: {}", path.display()))?;
        Self::load(&text)
    }

    /// Load a provider from a YAML string.
    pub fn load(yaml: &str) -> anyhow::Result<Self> {
        let config: MockConfig = serde_yaml::from_str(yaml)
            .context("parsing mock responses YAML")?;
        Ok(Self {
            config: Arc::new(config),
            call_count: Arc::new(Mutex::new(0)),
            name: "yaml-mock".into(),
        })
    }
}

#[async_trait]
impl crate::ModelProvider for YamlMockProvider {
    fn name(&self) -> &str { &self.name }
    fn model_name(&self) -> &str { "yaml-mock-model" }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let call_num = {
            let mut c = self.call_count.lock().unwrap();
            *c += 1;
            *c
        };

        // Determine whether we are responding after tool results were added.
        let has_tool_results = req.messages.iter()
            .any(|m| m.role == Role::Tool);

        // Find the last user message – this is the key we match against.
        let last_user_text = req.messages.iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.as_text())
            .unwrap_or("[no user message]")
            .to_string();

        debug!(call_num, has_tool_results, last_user = %last_user_text, "yaml mock complete()");

        let rule = self.find_rule(&last_user_text);

        let events = if has_tool_results {
            // Round 2: tool results are in – respond with after_tool_reply or reply
            let text = rule
                .and_then(|r| r.after_tool_reply.as_deref().or(r.reply.as_deref()))
                .unwrap_or("[no after-tool reply configured]");
            text_events(text)
        } else {
            match rule {
                None => text_events("[no mock rule matched]"),
                Some(r) if r.tool_calls.is_empty() => {
                    // Simple text response
                    let text = r.reply.as_deref().unwrap_or("[no reply configured]");
                    text_events(text)
                }
                Some(r) => {
                    // Emit tool calls
                    tool_call_events(&r.tool_calls)
                }
            }
        };

        Ok(Box::pin(stream::iter(events)))
    }
}

impl YamlMockProvider {
    fn find_rule<'a>(&'a self, user_text: &str) -> Option<&'a ResponseRule> {
        let lower = user_text.to_lowercase();
        let mut default_rule = None;

        for rule in &self.config.responses {
            match rule.match_type {
                MatchType::Default => {
                    default_rule = Some(rule);
                }
                MatchType::Contains => {
                    if lower.contains(&rule.pattern.to_lowercase()) {
                        return Some(rule);
                    }
                }
                MatchType::Equals => {
                    if lower == rule.pattern.to_lowercase() {
                        return Some(rule);
                    }
                }
                MatchType::StartsWith => {
                    if lower.starts_with(&rule.pattern.to_lowercase()) {
                        return Some(rule);
                    }
                }
                MatchType::Regex => {
                    if let Ok(re) = regex::Regex::new(&rule.pattern) {
                        if re.is_match(user_text) {
                            return Some(rule);
                        }
                    }
                }
            }
        }

        default_rule
    }
}

// ─── Event constructors ───────────────────────────────────────────────────────

fn text_events(text: &str) -> Vec<anyhow::Result<ResponseEvent>> {
    vec![
        Ok(ResponseEvent::TextDelta(text.to_string())),
        Ok(ResponseEvent::Usage { input_tokens: 5, output_tokens: text.len() as u32 / 4 + 1, cache_read_tokens: 0, cache_write_tokens: 0 }),
        Ok(ResponseEvent::Done),
    ]
}

fn tool_call_events(tool_calls: &[ToolCallDef]) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut events: Vec<anyhow::Result<ResponseEvent>> = tool_calls
        .iter()
        .map(|tc| Ok(ResponseEvent::ToolCall {
            id: tc.id.clone(),
            name: tc.tool.clone(),
            arguments: tc.args.to_string(),
        }))
        .collect();
    events.push(Ok(ResponseEvent::Done));
    events
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::{Message, ModelProvider};

    const BASIC_YAML: &str = r#"
responses:
  - match_type: equals
    pattern: "ping"
    reply: "pong"

  - match_type: contains
    pattern: "write"
    tool_calls:
      - id: tc-1
        tool: fs
        args:
          operation: write
          path: /tmp/test.txt
          content: hello
    after_tool_reply: "File written."

  - match_type: starts_with
    pattern: "plan"
    reply: "Here is the plan."

  - match_type: default
    reply: "default reply"
"#;

    fn provider() -> YamlMockProvider {
        YamlMockProvider::load(BASIC_YAML).unwrap()
    }

    fn req(user: &str) -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message::user(user)],
            stream: true,
            ..Default::default()
        }
    }

    fn req_with_tool_result(user: &str) -> CompletionRequest {
        CompletionRequest {
            messages: vec![
                Message::user(user),
                Message::tool_result("tc-1", "ok"),
            ],
            stream: true,
            ..Default::default()
        }
    }

    async fn collect(p: &YamlMockProvider, req: CompletionRequest) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        let mut stream = p.complete(req).await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev.unwrap());
        }
        events
    }

    // ── Match types ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn equals_match() {
        let p = provider();
        let events = collect(&p, req("ping")).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "pong")));
    }

    #[tokio::test]
    async fn contains_match_case_insensitive() {
        let p = provider();
        let events = collect(&p, req("Please WRITE the file")).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::ToolCall { name, .. } if name == "fs")));
    }

    #[tokio::test]
    async fn starts_with_match() {
        let p = provider();
        let events = collect(&p, req("plan the project")).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TextDelta(t) if t.contains("plan"))));
    }

    #[tokio::test]
    async fn default_fallback() {
        let p = provider();
        let events = collect(&p, req("something completely unrelated")).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "default reply")));
    }

    // ── Tool call sequence ────────────────────────────────────────────────────

    #[tokio::test]
    async fn round_1_emits_tool_call() {
        let p = provider();
        let events = collect(&p, req("write a file")).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::ToolCall { id, .. } if id == "tc-1")));
        // Should not emit text delta in round 1
        assert!(!events.iter().any(|e| matches!(e, ResponseEvent::TextDelta(_))));
    }

    #[tokio::test]
    async fn round_2_after_tool_result_emits_text() {
        let p = provider();
        // Simulate: first call emits tool call, second call (with tool result) emits text
        let events = collect(&p, req_with_tool_result("write a file")).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "File written.")));
    }

    #[tokio::test]
    async fn tool_call_contains_json_args() {
        let p = provider();
        let events = collect(&p, req("write the file")).await;
        let tc_event = events.iter().find_map(|e| {
            if let ResponseEvent::ToolCall { arguments, .. } = e { Some(arguments.as_str()) } else { None }
        });
        assert!(tc_event.is_some());
        let args_json = tc_event.unwrap();
        // Args should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(args_json).unwrap();
        assert_eq!(parsed["operation"].as_str(), Some("write"));
    }

    // ── Stream terminates ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn stream_always_ends_with_done() {
        let p = provider();
        for input in ["ping", "write something", "unknown"] {
            let events = collect(&p, req(input)).await;
            assert!(matches!(events.last(), Some(ResponseEvent::Done)),
                "stream for '{input}' should end with Done");
        }
    }

    // ── Loading from file ─────────────────────────────────────────────────────

    #[test]
    fn from_file_error_on_missing() {
        let result = YamlMockProvider::from_file("/nonexistent/path.yaml");
        assert!(result.is_err());
    }

    #[test]
    fn from_str_error_on_invalid_yaml() {
        let result = YamlMockProvider::load("{ invalid yaml: [");
        assert!(result.is_err());
    }

    #[test]
    fn from_str_ok_on_valid_yaml() {
        let result = YamlMockProvider::load(BASIC_YAML);
        assert!(result.is_ok());
    }

    // ── Regex match type ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn regex_match_type() {
        let yaml = r#"
responses:
  - match_type: regex
    pattern: "^step \\d+"
    reply: "step matched"
  - match_type: default
    reply: "no match"
"#;
        let p = YamlMockProvider::load(yaml).unwrap();
        let events = collect(&p, req("step 3 of the plan")).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "step matched")));
    }
}
