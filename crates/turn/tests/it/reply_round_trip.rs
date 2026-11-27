// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose recorded sessions can be
// trained on as they were sampled, for its clients. If your team needs
// expertise in agent trajectories as training data then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec (property): whatever a server streams and however it cuts the stream
//! into chunks, the next request sends the reply back as it arrived - its
//! text, its reasoning, each call's id, name and argument text (valid JSON or
//! not), and the server's `sampled` record byte for byte. The real
//! OpenAI-compatible driver against a local server, over many generated
//! replies.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sven_model::Message;
use sven_model_drivers::{from_config, ModelConfig};
use sven_turn::{stream_turn, TurnLimits};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// A small deterministic generator, so every case is reproducible by its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn text(&mut self, max: usize) -> String {
        const PIECES: [&str; 14] = [
            "a", "Z", " ", "\n", "\t", "\"", "\\", "{", "}", ":", ",", "é", "😀", "<think>",
        ];
        (0..self.below(max + 1))
            .map(|_| PIECES[self.below(PIECES.len())])
            .collect()
    }
    /// Argument text: JSON with odd key order and spacing, or text that is not JSON.
    fn arguments(&mut self) -> String {
        match self.below(4) {
            0 => format!("{{ \"z\" : {} ,\"a\":[1, 2.50] }}", json!(self.text(6))),
            1 => format!("{{\"cmd\":{}}}", json!(self.text(12))),
            2 => format!(
                "{{\"path\": \"\\{}\"",
                self.text(4).replace(['"', '\\'], "")
            ),
            _ => String::new(),
        }
    }
}

/// `text` cut into 1 to 4 pieces at character boundaries.
fn pieces(rng: &mut Lcg, text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut cuts: Vec<usize> = (0..rng.below(4))
        .map(|_| rng.below(chars.len() + 1))
        .collect();
    cuts.extend([0, chars.len()]);
    cuts.sort_unstable();
    cuts.windows(2)
        .map(|w| chars[w[0]..w[1]].iter().collect())
        .collect()
}

struct Reply {
    reasoning: String,
    text: String,
    calls: Vec<(String, String, String)>,
    sampled: String,
}

/// The SSE body that streams `reply` in arbitrary chunks.
fn stream_of(rng: &mut Lcg, reply: &Reply) -> String {
    let frame = |delta: Value| {
        format!(
            "data: {}\n\n",
            json!({"choices": [{"index": 0, "delta": delta}]})
        )
    };
    let mut body = String::new();
    for piece in pieces(rng, &reply.reasoning) {
        body += &frame(json!({"reasoning_content": piece}));
    }
    for piece in pieces(rng, &reply.text) {
        body += &frame(json!({"content": piece}));
    }
    for (index, (id, name, arguments)) in reply.calls.iter().enumerate() {
        body += &frame(
            json!({"tool_calls": [{"index": index, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]}),
        );
        for piece in pieces(rng, arguments) {
            body +=
                &frame(json!({"tool_calls": [{"index": index, "function": {"arguments": piece}}]}));
        }
    }
    body += &format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"sampled\":{}}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n", reply.sampled);
    body
}

/// A server that answers each request with the next of `answers`, recording each request's body.
async fn server(answers: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    tokio::spawn(async move {
        for answer in answers {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let body = loop {
                let mut chunk = [0u8; 8192];
                let n = socket.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len: usize = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + len {
                        break String::from_utf8(buf[head_end + 4..head_end + 4 + len].to_vec())
                            .unwrap();
                    }
                }
            };
            seen.lock().unwrap().push(body);
            let head = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", answer.len());
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(answer.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{addr}/v1"), bodies)
}

async fn turn(base: &str, messages: Vec<Message>) -> sven_turn::StreamedTurn {
    let provider = from_config(&ModelConfig {
        provider: "sven".into(),
        name: "m".into(),
        base_url: Some(base.into()),
        ..ModelConfig::default()
    })
    .unwrap();
    let (tx, mut rx) = mpsc::channel(4096);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let turn = stream_turn(
        provider.as_ref(),
        messages,
        vec![],
        None,
        None,
        None,
        TurnLimits::default(),
        &tx,
    )
    .await
    .unwrap();
    drop(tx);
    drain.await.unwrap();
    turn
}

#[tokio::test]
async fn the_next_request_sends_every_reply_back_as_it_arrived() {
    for seed in 1..=60u64 {
        let mut rng = Lcg(seed);
        let reply = Reply {
            reasoning: rng.text(8),
            text: rng.text(10),
            calls: (0..rng.below(4))
                .map(|i| {
                    (
                        format!("call_{seed}_{i}"),
                        format!("tool{i}"),
                        rng.arguments(),
                    )
                })
                .collect(),
            sampled: format!(
                "{{\"token_ids\":\"AQI{}\" , \"finish\":\"stop\",\"context_len\":{seed}}}",
                rng.below(9)
            ),
        };
        if reply.text.is_empty() && reply.calls.is_empty() && reply.reasoning.is_empty() {
            continue;
        }
        let (base, bodies) =
            server(vec![stream_of(&mut rng, &reply), "data: [DONE]\n\n".into()]).await;
        let user = Message::user("go");
        let first = turn(&base, vec![user.clone()]).await;
        let mut history = vec![user];
        history.extend(sven_model::reply_messages(&first.reply));
        for call in &first.reply.calls {
            history.push(Message::tool_result(&call.id, "ok"));
        }
        turn(&base, history).await;
        let sent = bodies.lock().unwrap()[1].clone();
        let request: Value = serde_json::from_str(&sent).unwrap();
        let answer = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .unwrap_or_else(|| panic!("seed {seed}: {sent}"));
        assert_eq!(
            answer["content"].as_str().unwrap_or(""),
            reply.text,
            "seed {seed}"
        );
        assert_eq!(
            answer["reasoning_content"].as_str().unwrap_or(""),
            reply.reasoning,
            "seed {seed}"
        );
        let calls: Vec<(String, String, String)> = answer["tool_calls"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| {
                        (
                            c["id"].as_str().unwrap().into(),
                            c["function"]["name"].as_str().unwrap().into(),
                            c["function"]["arguments"].as_str().unwrap().into(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(calls, reply.calls, "seed {seed}");
        assert!(
            sent.contains(&format!("\"sampled\":{}", reply.sampled)),
            "seed {seed}: {sent}"
        );
    }
}
