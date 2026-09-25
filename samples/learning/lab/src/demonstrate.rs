// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! A demonstration with real observations.
//!
//! The model cannot yet solve the task, so there are no successful episodes to
//! learn from. A demonstration supplies them - but a demonstration is only
//! worth training on if everything except the decisions is real. Invent the
//! tool output and the model learns to expect a format it will never be shown;
//! invent the prompt and it learns a context it never meets.
//!
//! So this scripts only the decisions, and scripts them where the model would
//! have made them: at the wire. It speaks the completion protocol back to the
//! agent, returning the tool call the demonstration calls for. Everything
//! downstream is the real thing - the real tool executor runs the call, the
//! real formatter renders its output, the real loop decides what to send next,
//! and the real verifier judges the result afterwards. The agent cannot tell
//! it is being led, which is the point: what comes back is a genuine
//! trajectory whose only synthetic part is the choice of action.
//!
//! What this is not: evidence that the model can solve anything. A
//! demonstration teaches one path. It is recorded as [`Provenance::Scripted`]
//! so it can never be counted as the model improving on its own, and a claim
//! about self-improvement needs the model's own successes, not these.
//!
//! [`Provenance::Scripted`]: crate::Provenance

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// One decision in a demonstration.
#[derive(Clone, Debug)]
pub enum Step {
    /// Call a tool. `arguments` is JSON text, as a model would emit it.
    Call { name: String, arguments: String },
    /// Finish with an answer.
    Say { text: String },
}

impl Step {
    pub fn call(name: &str, arguments: &str) -> Step {
        Step::Call {
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }
    pub fn say(text: &str) -> Step {
        Step::Say {
            text: text.to_string(),
        }
    }
}

/// A scripted model, listening on an ephemeral local port.
pub struct Demonstrator {
    port: u16,
    state: Arc<Mutex<State>>,
}

struct State {
    steps: Vec<Step>,
    next: usize,
    requests: Vec<serde_json::Value>,
}

impl Demonstrator {
    /// Start answering with `steps`, in order, one per request.
    pub async fn start(steps: Vec<Step>) -> std::io::Result<Demonstrator> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(State {
            steps,
            next: 0,
            requests: Vec::new(),
        }));

        let shared = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    break;
                };
                let shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    let _ = answer(client, shared).await;
                });
            }
        });

        Ok(Demonstrator { port, state })
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// Every request the agent made, in order.
    ///
    /// The LAST one is the useful one: its `messages` array is the whole
    /// conversation the model was shown, including every real tool result, in
    /// the exact shape the server was given it.
    pub async fn requests(&self) -> Vec<serde_json::Value> {
        self.state.lock().await.requests.clone()
    }

    /// How many scripted decisions were actually consumed. Fewer than the
    /// script means the agent stopped early; more requests than steps means it
    /// kept going after the demonstration ran out, and the trajectory is not
    /// the one that was intended.
    pub async fn consumed(&self) -> usize {
        self.state.lock().await.next
    }
}

async fn answer(mut client: TcpStream, state: Arc<Mutex<State>>) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let length = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let mut chunk = [0u8; 8192];
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = &buffer[head_end..(head_end + length).min(buffer.len())];

    // Only a completion consumes a decision. The agent probes the provider
    // before it starts (a model listing, to resolve which model it is talking
    // to), and answering that with the first scripted step silently shifts the
    // whole demonstration by one: the episode then begins in the middle of the
    // script, which is how this first showed up - as a write with no prior
    // look.
    let target = head.lines().next().unwrap_or_default().to_string();
    if !target.contains("chat/completions") {
        let listing = serde_json::json!({
            "object": "list",
            "data": [{ "id": "demonstration", "object": "model", "owned_by": "lab" }]
        })
        .to_string();
        client
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n",
                    listing.len()
                )
                .as_bytes(),
            )
            .await?;
        client.write_all(listing.as_bytes()).await?;
        client.flush().await?;
        return Ok(());
    }

    let step = {
        let mut state = state.lock().await;
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
            state.requests.push(json);
        }
        let step = state.steps.get(state.next).cloned();
        if step.is_some() {
            state.next += 1;
        }
        step
    };

    // Out of script: end the turn rather than repeating the last decision,
    // which would loop the agent forever.
    let step = step.unwrap_or_else(|| Step::say("done"));
    let payload = sse_for(&step);

    client
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                 Connection: close\r\nContent-Length: {}\r\n\r\n",
                payload.len()
            )
            .as_bytes(),
        )
        .await?;
    client.write_all(payload.as_bytes()).await?;
    client.flush().await?;
    Ok(())
}

/// Render one decision as the completion stream a provider would send.
fn sse_for(step: &Step) -> String {
    let (delta, finish) = match step {
        Step::Call { name, arguments } => (
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call_{}", short_id(name, arguments)),
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]
            }),
            "tool_calls",
        ),
        Step::Say { text } => (serde_json::json!({ "content": text }), "stop"),
    };

    let first = serde_json::json!({
        "id": "chatcmpl-demo",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "demonstration",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": serde_json::Value::Null }]
    });
    let last = serde_json::json!({
        "id": "chatcmpl-demo",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "demonstration",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }]
    });

    format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n")
}

/// A stable id per call, so a re-run of the same demonstration produces the
/// same correlation ids and two datasets can be compared byte for byte.
fn short_id(name: &str, arguments: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in name.bytes().chain(arguments.bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_is_rendered_as_a_completion_stream_that_ends_in_tool_calls() {
        let sse = sse_for(&Step::call("shell", r#"{"command":"ls"}"#));
        assert!(sse.starts_with("data: "), "{sse}");
        assert!(sse.ends_with("data: [DONE]\n\n"), "{sse}");
        assert!(sse.contains("\"finish_reason\":\"tool_calls\""), "{sse}");
        // Arguments travel as the JSON TEXT a model emits, not as an object.
        assert!(sse.contains(r#"\"command\":\"ls\""#), "{sse}");
    }

    #[test]
    fn an_answer_ends_the_turn() {
        let sse = sse_for(&Step::say("all done"));
        assert!(sse.contains("\"finish_reason\":\"stop\""), "{sse}");
        assert!(sse.contains("all done"), "{sse}");
    }

    #[test]
    fn the_same_call_always_gets_the_same_id() {
        // Two runs of one demonstration must produce comparable datasets.
        assert_eq!(
            short_id("shell", r#"{"command":"ls"}"#),
            short_id("shell", r#"{"command":"ls"}"#)
        );
        assert_ne!(
            short_id("shell", "{}"),
            short_id("shell", r#"{"command":"ls"}"#)
        );
    }

    /// The episode that exposed this made exactly two tool calls and then
    /// waited forever. Two is not a number a protocol should care about, so
    /// the question is whether the server keeps answering at all.
    #[tokio::test]
    async fn every_request_is_answered_not_just_the_first_few() {
        let steps: Vec<Step> = (0..6)
            .map(|i| Step::call("shell", &format!("{{\"n\":{i}}}")))
            .collect();
        let demo = Demonstrator::start(steps).await.expect("demonstrator");

        for round in 0..6 {
            let mut client = TcpStream::connect(format!("127.0.0.1:{}", demo.port))
                .await
                .unwrap_or_else(|e| panic!("connect on round {round}: {e}"));
            let body = r#"{"model":"m","messages":[],"stream":true}"#;
            client
                .write_all(
                    format!(
                        "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write");
            let mut answer = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                client.read_to_string(&mut answer),
            )
            .await
            .unwrap_or_else(|_| panic!("round {round} was never answered"))
            .expect("read");
            assert!(
                answer.contains(&format!("{{\\\"n\\\":{round}}}")),
                "round {round}: {answer}"
            );
        }
        assert_eq!(demo.consumed().await, 6);
    }

    #[tokio::test]
    async fn the_agent_is_answered_step_by_step_and_the_requests_are_kept() {
        let demo = Demonstrator::start(vec![
            Step::call("shell", r#"{"command":"./svctl status"}"#),
            Step::say("staging is live"),
        ])
        .await
        .expect("demonstrator");

        for expected in ["tool_calls", "stop"] {
            let mut client = TcpStream::connect(format!("127.0.0.1:{}", demo.port))
                .await
                .expect("connect");
            let body = r#"{"model":"m","messages":[],"stream":true}"#;
            client
                .write_all(
                    format!(
                        "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write");
            let mut answer = String::new();
            client.read_to_string(&mut answer).await.expect("read");
            assert!(answer.contains(expected), "expected {expected} in {answer}");
        }

        assert_eq!(demo.consumed().await, 2, "both decisions must be used");
        assert_eq!(demo.requests().await.len(), 2, "every request is kept");
    }
}
