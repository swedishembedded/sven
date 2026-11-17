// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a sampling control is honoured, refused, or sent knowingly
//! best-effort; never silently ignored.
//!
//! `RunOptions::sampling` promises that a provider which cannot honour a
//! control refuses the request. A provider declares the controls it honours;
//! a run asking for one it does not declare fails with a typed error naming
//! the control before anything is sent, unless the model's configuration
//! allows best effort. Then the controls are sent, the run warns once, and
//! the trajectory says which controls were honoured and which only sent, so
//! a learner's "seeded" can mean the seed took effect.
//!
//! Swedish Embedded AB implements reproducible, training-grade agent runs for
//! its clients. If your team needs expertise in deterministic sampling across
//! model servers then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use sven_sdk::{
    config::Config,
    model::{SamplingControl, SamplingParams},
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    CallError, Engine, RunOptions,
};
use sven_vocab::SessionEvent;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A loopback OpenAI-compatible server: lists one model and answers chat
/// completions from a script, recording every request body it was sent.
struct Server {
    base_url: String,
    chats: Arc<Mutex<Vec<Value>>>,
}

impl Server {
    /// `replies` are SSE bodies, one per chat request in order.
    async fn start(replies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let chats = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&chats);
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = Arc::clone(&seen);
                let replies = Arc::clone(&replies);
                tokio::spawn(async move {
                    let (read_half, mut write_half) = stream.into_split();
                    let mut reader = BufReader::new(read_half);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap_or(0);
                    let request_line = line.clone();
                    let mut length = 0usize;
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                            break;
                        }
                        if line.trim().is_empty() {
                            break;
                        }
                        if let Some((k, v)) = line.trim().split_once(": ") {
                            if k.eq_ignore_ascii_case("content-length") {
                                length = v.parse().unwrap_or(0);
                            }
                        }
                    }
                    let mut body = vec![0u8; length];
                    let _ = reader.read_exact(&mut body).await;
                    let (status, kind, reply) =
                        if request_line.starts_with("POST /v1/chat/completions") {
                            seen.lock()
                                .unwrap()
                                .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                            let next = replies.lock().unwrap().next();
                            match next {
                                Some(sse) => (200, "text/event-stream", sse),
                                None => (500, "application/json", "{}".to_string()),
                            }
                        } else if request_line.starts_with("GET /v1/models") {
                            (
                                200,
                                "application/json",
                                r#"{"object":"list","data":[{"id":"m"}]}"#.to_string(),
                            )
                        } else {
                            (404, "application/json", "{}".to_string())
                        };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    let _ = write_half.write_all(response.as_bytes()).await;
                });
            }
        });
        Self { base_url, chats }
    }

    fn chat_bodies(&self) -> Vec<Value> {
        self.chats.lock().unwrap().clone()
    }
}

fn sse(frames: &[String]) -> String {
    let mut body: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    body
}

fn usage_frame(extra: &str) -> String {
    format!(
        r#"{{"model":"m",{extra}"choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":10,"completion_tokens":1}}}}"#
    )
}

/// A text answer; a brain-like server echoes the seed it used.
fn answer(echo_seed: Option<u64>) -> String {
    let echo = echo_seed.map_or(String::new(), |s| format!(r#""brain":{{"seed":{s}}},"#));
    sse(&[
        r#"{"model":"m","choices":[{"delta":{"content":"Done."}}]}"#.to_string(),
        usage_frame(&echo),
    ])
}

/// A call of the `probe` tool, so a run makes two requests.
fn probe_call() -> String {
    sse(&[
        r#"{"model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"probe","arguments":"{}"}}]}}]}"#.to_string(),
        r#"{"model":"m","choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#.to_string(),
    ])
}

struct Probe;

#[async_trait::async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        "probe"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Reads the probe."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "steady")
    }
}

fn seeded() -> RunOptions {
    RunOptions::new().sampling(SamplingParams {
        temperature: Some(0.2),
        top_p: None,
        top_k: Some(40),
        seed: Some(7),
    })
}

fn engine(server: &Server, provider: &str, best_effort: bool) -> Engine {
    let mut config = Config::default();
    config.model.provider = provider.into();
    config.model.name = "m".into();
    config.model.api_key = Some("key".into());
    config.model.base_url = Some(server.base_url.clone());
    config.model.sampling_best_effort = best_effort;
    let provider = sven_sdk::drivers::from_config(&config.model).expect("a provider");
    Engine::builder()
        .config(config)
        .model_provider(Arc::from(provider))
        .tool(Arc::new(Probe))
        .build()
        .expect("an engine builds")
}

/// `extra.sven.<key>` of every agent step that carries metrics.
fn step_facts(agent: &sven_sdk::Agent, key: &str) -> Vec<Value> {
    agent
        .trajectory()
        .steps
        .iter()
        .filter_map(|s| s.metrics.as_ref()?.extra.as_ref())
        .map(|extra| extra["sven"][key].clone())
        .collect()
}

#[tokio::test]
async fn a_generic_server_refuses_controls_it_may_ignore_before_sending() {
    let server = Server::start(vec![answer(None)]).await;
    let mut agent = engine(&server, "openai", false).agent("agent");
    let error = agent
        .send_with("hi", seeded())
        .await
        .expect_err("the run is refused");
    let CallError::SamplingRefused(refused) = &error else {
        panic!("a typed refusal, not {error:?}");
    };
    let named: Vec<SamplingControl> = refused.controls.iter().collect();
    assert_eq!(named, vec![SamplingControl::TopK, SamplingControl::Seed]);
    let text = error.to_string();
    assert!(text.contains("top_k") && text.contains("seed"), "{text}");
    assert!(server.chat_bodies().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn best_effort_sends_them_warns_once_and_records_them_as_unhonoured() {
    let server = Server::start(vec![probe_call(), answer(None)]).await;
    let mut agent = engine(&server, "openai", true).agent("agent");
    let mut events = agent.events();
    agent.send_with("hi", seeded()).await.expect("an outcome");

    let bodies = server.chat_bodies();
    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        assert_eq!(body["seed"], 7, "sent: {body}");
        assert_eq!(body["top_k"], 40, "sent: {body}");
    }
    let mut warnings = 0;
    while let Ok(Some(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await
    {
        if let SessionEvent::Warning(text) = &event.event {
            if text.contains("seed") {
                warnings += 1;
            }
        }
    }
    assert_eq!(warnings, 1, "one warning for the run");
    let unhonoured = step_facts(&agent, "sampling_unhonoured");
    let honoured = step_facts(&agent, "sampling_honoured");
    assert_eq!(unhonoured.len(), 2, "both requests are on steps");
    for (sent, took) in unhonoured.iter().zip(&honoured) {
        assert_eq!(sent, &serde_json::json!(["top_k", "seed"]));
        assert_eq!(took, &serde_json::json!(["temperature"]));
    }
    // A seed nobody says was used is not recorded as the seed in effect.
    assert!(step_facts(&agent, "seed").iter().all(Value::is_null));
}

#[tokio::test]
async fn a_brain_server_honours_seed_and_top_k() {
    let server = Server::start(vec![answer(Some(7))]).await;
    let mut agent = engine(&server, "brain", false).agent("agent");
    agent.send_with("hi", seeded()).await.expect("an outcome");

    let body = &server.chat_bodies()[0];
    assert_eq!(body["seed"], 7);
    assert_eq!(body["top_k"], 40);
    assert_eq!(
        step_facts(&agent, "sampling_honoured"),
        vec![serde_json::json!(["temperature", "top_k", "seed"])]
    );
    assert!(step_facts(&agent, "sampling_unhonoured")
        .iter()
        .all(Value::is_null));
    assert_eq!(step_facts(&agent, "seed"), vec![serde_json::json!(7)]);
}

#[tokio::test]
async fn best_effort_counts_a_seed_the_server_echoes_as_honoured() {
    let server = Server::start(vec![answer(Some(7))]).await;
    let mut agent = engine(&server, "openai", true).agent("agent");
    agent.send_with("hi", seeded()).await.expect("an outcome");
    assert_eq!(
        step_facts(&agent, "sampling_honoured"),
        vec![serde_json::json!(["temperature", "seed"])]
    );
    assert_eq!(
        step_facts(&agent, "sampling_unhonoured"),
        vec![serde_json::json!(["top_k"])]
    );
}
