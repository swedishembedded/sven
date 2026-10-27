// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
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
///           path: test.txt
///           content: "hello"
///     after_tool_reply: "File written."
///
///   - match_type: default
///     reply: "I understand your request."
///
///   - match_type: contains
///     pattern: "trigger provider error"
///     error: "the model failed to process the request"
/// ```
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use async_trait::async_trait;
use futures::stream;
use serde::{Deserialize, Serialize};
use tracing::debug;

use sven_model::{CompletionRequest, ResponseEvent, ResponseStream, Role};

use crate::recovery::RecoverRule;

// ─── YAML schema ─────────────────────────────────────────────────────────────

/// Root document.
#[derive(Debug, Deserialize)]
pub struct MockConfig {
    pub responses: Vec<ResponseRule>,
    /// When set, every request the provider receives is appended to this file
    /// as one JSON line: the tools offered and every message, in full. It is
    /// the record of exactly what a model would have read, for tests that
    /// check what reached it.
    #[serde(default)]
    pub request_log: Option<std::path::PathBuf>,
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
    /// Optional thinking / chain-of-thought content emitted as
    /// `ThinkingDelta` events before the `reply` text.  Used to test that
    /// the CI runner and TUI correctly surface thinking blocks.
    pub thinking: Option<String>,
    /// Tool calls to emit in the first round.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDef>,
    /// Text reply to send after tool results arrive (second round).
    pub after_tool_reply: Option<String>,
    /// When set, `complete()` returns a stream that yields a single `Err`
    /// carrying this message instead of any response event - simulating a
    /// provider that fails mid-stream (e.g. an OpenAI-compatible server that
    /// admits a request with `HTTP 200` + SSE headers, then rejects it, as
    /// brain does when a prompt exceeds its serving capacity). Used to
    /// exercise the fail-loudly path end-to-end: `[sven:error]` on stderr,
    /// `success=false` in the step trace, non-zero exit.
    pub error: Option<String>,
    /// For a rule that only replies: follow the references in the request
    /// back to their raw text before replying (see [`RecoverRule`]).
    #[serde(default)]
    pub recover: Option<RecoverRule>,
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
        let config: MockConfig =
            serde_yaml::from_str(yaml).context("parsing mock responses YAML")?;
        for rule in &config.responses {
            if let Some(recover) = &rule.recover {
                recover.validate().context("parsing mock responses YAML")?;
            }
        }
        Ok(Self {
            config: Arc::new(config),
            call_count: Arc::new(Mutex::new(0)),
            name: "yaml-mock".into(),
        })
    }
}

#[async_trait]
impl sven_model::ModelProvider for YamlMockProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn model_name(&self) -> &str {
        "yaml-mock-model"
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let call_num = {
            let mut c = self.call_count.lock().unwrap();
            *c += 1;
            *c
        };

        // Whether the current instruction's tool round has come back: a tool
        // result after the last user message. Results answering an earlier
        // instruction do not count, so every instruction gets its own round.
        let has_tool_results = req
            .messages
            .iter()
            .rev()
            .take_while(|m| m.role != Role::User)
            .any(|m| m.role == Role::Tool);

        // Find the last user message - this is the key we match against.
        let last_user_text = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.as_text())
            .unwrap_or("[no user message]")
            .to_string();

        debug!(call_num, has_tool_results, last_user = %last_user_text, "yaml mock complete()");

        if let Some(path) = &self.config.request_log {
            append_request_log(path, call_num, &req)?;
        }

        let rule = self.find_rule(&last_user_text);

        if let Some(msg) = rule.and_then(|r| r.error.as_deref()) {
            let err: anyhow::Result<ResponseEvent> = Err(anyhow::anyhow!("{msg}"));
            return Ok(Box::pin(stream::iter(vec![err])));
        }

        // The scripted model follows the instruction a real one is given
        // under deferred tool disclosure: a tool whose definition is not in
        // the request is loaded through the `tools` tool before it is called.
        // With every tool offered this never applies.
        let called = tool_call_ids_since_last_user(&req);
        let only_loaded_so_far = !called.is_empty() && called.iter().all(|id| *id == LOAD_CALL_ID);
        let events = match rule {
            Some(r) if has_tool_results && only_loaded_so_far && !r.tool_calls.is_empty() => {
                // The load has come back: make the scripted calls.
                tool_call_events(&r.tool_calls)
            }
            _ if has_tool_results => {
                // Round 2: tool results are in - respond with after_tool_reply or reply
                let text = rule
                    .and_then(|r| r.after_tool_reply.as_deref().or(r.reply.as_deref()))
                    .unwrap_or("[no after-tool reply configured]");
                text_events(text, None)
            }
            None => text_events("[no mock rule matched]", None),
            Some(r) if r.tool_calls.is_empty() => {
                let recovery = r
                    .recover
                    .as_ref()
                    .map(|x| x.calls(&req))
                    .unwrap_or_default();
                if recovery.is_empty() {
                    // Simple text response (optionally preceded by thinking)
                    let text = r.reply.as_deref().unwrap_or("[no reply configured]");
                    text_events(text, r.thinking.as_deref())
                } else {
                    tool_call_events(&recovery)
                }
            }
            Some(r) => match unloaded_tools(r, &req) {
                missing if !missing.is_empty() => load_call_events(&missing),
                _ => tool_call_events(&r.tool_calls),
            },
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

/// One JSON line of the request log: `{"request": n, "pid", "tools": [names],
/// "messages": [{"role", "kind": text|tool_call|tool_result, "name"?, "text"}]}`.
fn append_request_log(path: &Path, call_num: u32, req: &CompletionRequest) -> anyhow::Result<()> {
    use std::io::Write;

    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = format!("{:?}", m.role).to_lowercase();
            match &m.content {
                sven_model::MessageContent::Text(t) => {
                    serde_json::json!({"role": role, "kind": "text", "text": t})
                }
                sven_model::MessageContent::ContentParts(parts) => {
                    let text: Vec<&str> = parts
                        .iter()
                        .filter_map(|p| match p {
                            sven_model::ContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    serde_json::json!({"role": role, "kind": "text", "text": text.join("\n")})
                }
                sven_model::MessageContent::ToolCall { function, .. } => serde_json::json!({
                    "role": role, "kind": "tool_call",
                    "name": function.name, "text": function.arguments,
                }),
                sven_model::MessageContent::ToolResult { content, .. } => serde_json::json!({
                    "role": role, "kind": "tool_result",
                    "text": content.as_text().unwrap_or("[non-text content]"),
                }),
            }
        })
        .collect();
    let line = serde_json::json!({
        "request": call_num,
        "pid": std::process::id(),
        "tools": req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        "messages": messages,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening the mock request log: {}", path.display()))?;
    // One write per record: processes that share the log (a parent and its
    // sub-agents) append whole lines, never fragments.
    let record = format!("{line}\n");
    file.write_all(record.as_bytes())
        .with_context(|| format!("writing the mock request log: {}", path.display()))
}

// ─── Event constructors ───────────────────────────────────────────────────────

fn text_events(text: &str, thinking: Option<&str>) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut events: Vec<anyhow::Result<ResponseEvent>> = Vec::new();
    // Emit thinking deltas before the text response, if the rule specifies it.
    if let Some(thought) = thinking {
        events.push(Ok(ResponseEvent::ThinkingDelta(thought.to_string())));
    }
    events.push(Ok(ResponseEvent::TextDelta(text.to_string())));
    events.push(Ok(ResponseEvent::Usage {
        input_tokens: 5,
        output_tokens: text.len() as u32 / 4 + 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: None,
    }));
    events.push(Ok(ResponseEvent::Done));
    events
}

/// The tools `rule` calls that the request does not carry a definition of,
/// when the request carries the discovery tool that could load them. Empty
/// when every tool is offered, or when nothing could load the missing ones.
fn unloaded_tools(rule: &ResponseRule, req: &CompletionRequest) -> Vec<String> {
    const DISCOVERY: &str = "tools";
    if !req.tools.iter().any(|t| t.name == DISCOVERY) {
        return Vec::new();
    }
    let mut missing: Vec<String> = Vec::new();
    for call in &rule.tool_calls {
        let offered = req.tools.iter().any(|t| t.name == call.tool);
        if !offered && !missing.contains(&call.tool) {
            missing.push(call.tool.clone());
        }
    }
    missing
}

/// The id of the call the mock makes to load a tool.
const LOAD_CALL_ID: &str = "mock-load";

/// A single call that loads `names` through the discovery tool.
fn load_call_events(names: &[String]) -> Vec<anyhow::Result<ResponseEvent>> {
    let args = serde_json::json!({"action": "load", "names": names});
    vec![
        Ok(ResponseEvent::ToolCall {
            index: 0,
            id: LOAD_CALL_ID.to_string(),
            name: "tools".to_string(),
            arguments: args.to_string(),
        }),
        Ok(ResponseEvent::Done),
    ]
}

/// The ids of the tool calls made after the last user message. A load the
/// mock made itself is told apart from a scripted call to the same tool by
/// its id.
fn tool_call_ids_since_last_user(req: &CompletionRequest) -> Vec<&str> {
    req.messages
        .iter()
        .rev()
        .take_while(|m| m.role != Role::User)
        .filter_map(|m| match &m.content {
            sven_model::MessageContent::ToolCall { tool_call_id, .. } => {
                Some(tool_call_id.as_str())
            }
            _ => None,
        })
        .collect()
}

fn tool_call_events(tool_calls: &[ToolCallDef]) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut events: Vec<anyhow::Result<ResponseEvent>> = tool_calls
        .iter()
        .zip(0u32..)
        .map(|(tc, index)| {
            Ok(ResponseEvent::ToolCall {
                index,
                id: tc.id.clone(),
                name: tc.tool.clone(),
                arguments: tc.args.to_string(),
            })
        })
        .collect();
    events.push(Ok(ResponseEvent::Done));
    events
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use sven_model::{Message, ModelProvider};

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
          path: test.txt
          content: hello
    after_tool_reply: "File written."

  - match_type: starts_with
    pattern: "plan"
    reply: "Here is the plan."

  - match_type: default
    reply: "default reply"

  - match_type: equals
    pattern: "trigger provider error"
    error: "the model failed to process the request"
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
            messages: vec![Message::user(user), Message::tool_result("tc-1", "ok")],
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

    // ── Request log ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn the_request_log_records_what_the_model_was_sent() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("requests.jsonl");
        let yaml = format!(
            "request_log: {}\nresponses:\n  - match_type: default\n    reply: ok\n",
            log.display()
        );
        let p = YamlMockProvider::load(&yaml).unwrap();
        collect(&p, req("first question")).await;
        collect(&p, req_with_tool_result("second question")).await;

        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "one line per request");
        assert_eq!(lines[0]["request"], 1);
        assert_eq!(lines[0]["messages"][0]["text"], "first question");
        assert_eq!(lines[1]["request"], 2);
        assert_eq!(lines[1]["messages"][1]["kind"], "tool_result");
        assert_eq!(lines[1]["messages"][1]["text"], "ok");
    }

    #[tokio::test]
    async fn a_scripted_call_to_the_discovery_tool_is_not_mistaken_for_a_load() {
        use sven_model::{FunctionCall, MessageContent, ToolSchema};
        let yaml = "responses:\n  - match_type: contains\n    pattern: find\n    tool_calls:\n      - id: s-1\n        tool: tools\n        args: {action: search, query: x}\n    after_tool_reply: searched\n";
        let p = YamlMockProvider::load(yaml).unwrap();
        let tools = vec![ToolSchema {
            name: "tools".into(),
            description: String::new(),
            parameters: serde_json::json!({}),
            ..Default::default()
        }];
        let first = CompletionRequest {
            messages: vec![Message::user("find it")],
            tools: tools.clone(),
            stream: true,
            ..Default::default()
        };
        assert!(collect(&p, first)
            .await
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCall { id, .. } if id == "s-1")));
        let call = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "s-1".into(),
                function: FunctionCall {
                    name: "tools".into(),
                    arguments: "{}".into(),
                },
            },
            reasoning: None,
        };
        let again = CompletionRequest {
            messages: vec![
                Message::user("find it"),
                call,
                Message::tool_result("s-1", "found"),
            ],
            tools,
            stream: true,
            ..Default::default()
        };
        let events = collect(&p, again).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "searched")),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn without_a_request_log_nothing_is_written() {
        let p = provider();
        collect(&p, req("ping")).await;
    }

    // ── Match types ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn equals_match() {
        let p = provider();
        let events = collect(&p, req("ping")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "pong")));
    }

    #[tokio::test]
    async fn contains_match_case_insensitive() {
        let p = provider();
        let events = collect(&p, req("Please WRITE the file")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCall { name, .. } if name == "fs")));
    }

    #[tokio::test]
    async fn starts_with_match() {
        let p = provider();
        let events = collect(&p, req("plan the project")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t.contains("plan"))));
    }

    #[tokio::test]
    async fn default_fallback() {
        let p = provider();
        let events = collect(&p, req("something completely unrelated")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "default reply")));
    }

    // ── Error simulation ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn error_rule_yields_err_instead_of_response_event() {
        let p = provider();
        let mut stream = p.complete(req("trigger provider error")).await.unwrap();
        let first = stream.next().await.expect("stream must yield one item");
        let err = first.expect_err("error rule must yield Err, not a ResponseEvent");
        assert!(
            err.to_string()
                .contains("the model failed to process the request"),
            "unexpected error message: {err}"
        );
        // Exactly one item - a stream that fails mid-request has nothing more to say.
        assert!(stream.next().await.is_none());
    }

    // ── Deferred tool disclosure ──────────────────────────────────────────────

    fn offering(tools: &[&str], mut req: CompletionRequest) -> CompletionRequest {
        req.tools = tools
            .iter()
            .map(|name| sven_model::ToolSchema {
                name: (*name).to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
                is_mcp: false,
            })
            .collect();
        req
    }

    fn calls(events: &[ResponseEvent]) -> Vec<(&str, &str)> {
        events
            .iter()
            .filter_map(|e| match e {
                ResponseEvent::ToolCall {
                    name, arguments, ..
                } => Some((name.as_str(), arguments.as_str())),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_tool_that_is_not_offered_is_loaded_before_it_is_called() {
        let p = provider();
        let events = collect(&p, offering(&["tools", "shell"], req("write a file"))).await;
        let made = calls(&events);
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].0, "tools");
        assert!(made[0].1.contains(r#""names":["fs"]"#), "{made:?}");
    }

    #[tokio::test]
    async fn the_scripted_calls_follow_once_the_load_has_come_back() {
        let p = provider();
        let mut request = offering(&["tools", "fs"], req("write a file"));
        request.messages.push(Message {
            role: Role::Assistant,
            content: sven_model::MessageContent::ToolCall {
                tool_call_id: "mock-load".into(),
                function: sven_model::FunctionCall {
                    name: "tools".into(),
                    arguments: "{}".into(),
                },
            },
            reasoning: None,
        });
        request
            .messages
            .push(Message::tool_result("mock-load", "Loaded: fs"));
        let events = collect(&p, request).await;
        assert_eq!(calls(&events)[0].0, "fs");
    }

    #[tokio::test]
    async fn an_offered_tool_is_called_at_once() {
        let p = provider();
        let events = collect(&p, offering(&["tools", "fs"], req("write a file"))).await;
        assert_eq!(calls(&events)[0].0, "fs");
    }

    // ── Tool call sequence ────────────────────────────────────────────────────

    #[tokio::test]
    async fn round_1_emits_tool_call() {
        let p = provider();
        let events = collect(&p, req("write a file")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCall { id, .. } if id == "tc-1")));
        // Should not emit text delta in round 1
        assert!(!events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextDelta(_))));
    }

    #[tokio::test]
    async fn round_2_after_tool_result_emits_text() {
        let p = provider();
        // Simulate: first call emits tool call, second call (with tool result) emits text
        let events = collect(&p, req_with_tool_result("write a file")).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "File written.")));
    }

    /// Several scripted calls stream as several calls: each on its own index,
    /// as a streaming provider numbers parallel calls.
    #[tokio::test]
    async fn parallel_calls_stream_on_their_own_indices() {
        let p = YamlMockProvider::load(
            r#"
responses:
  - match_type: default
    tool_calls:
      - { id: a, tool: one, args: {} }
      - { id: b, tool: two, args: {} }
"#,
        )
        .unwrap();
        let indices: Vec<u32> = collect(&p, req("go"))
            .await
            .into_iter()
            .filter_map(|e| match e {
                ResponseEvent::ToolCall { index, .. } => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(indices, vec![0, 1]);
    }

    /// A new instruction later in the conversation starts its own tool round:
    /// only results that answer the current instruction end it.
    #[tokio::test]
    async fn a_later_instruction_calls_its_tools_again() {
        let p = provider();
        let later = CompletionRequest {
            messages: vec![
                Message::user("write it"),
                Message::tool_result("tc-1", "ok"),
                Message::assistant("File written."),
                Message::user("write another"),
            ],
            stream: true,
            ..Default::default()
        };
        let events = collect(&p, later).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ResponseEvent::ToolCall { .. })),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn tool_call_contains_json_args() {
        let p = provider();
        let events = collect(&p, req("write the file")).await;
        let tc_event = events.iter().find_map(|e| {
            if let ResponseEvent::ToolCall { arguments, .. } = e {
                Some(arguments.as_str())
            } else {
                None
            }
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
            assert!(
                matches!(events.last(), Some(ResponseEvent::Done)),
                "stream for '{input}' should end with Done"
            );
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
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "step matched")));
    }
}
