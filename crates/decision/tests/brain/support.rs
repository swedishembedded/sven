// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A loopback stand-in for the decision provider's HTTP surface: it records
//! what it receives, answers from a handler and can hold requests open.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The recorded wire, as the provider's contract fixtures.
pub const REQUEST: &str = include_str!("../fixtures/brain/run_decide.request.json");
pub const DECIDE: &str = include_str!("../fixtures/brain/run_decide.response.json");
pub const DECIDE_FLAGS: &str = include_str!("../fixtures/brain/run_decide_flags.response.json");
pub const MODELS: &str = include_str!("../fixtures/brain/models.response.json");
pub const SELECT: &str = include_str!("../fixtures/brain/select.response.json");
pub const SELECT_NONE: &str = include_str!("../fixtures/brain/select_none.response.json");

/// One request received.
#[derive(Debug, Clone)]
pub struct Received {
    pub path: String,
    pub authorization: Option<String>,
    pub body: Value,
}

impl Received {
    pub fn model(&self) -> &str {
        self.body["model"].as_str().unwrap_or_default()
    }
    pub fn action(&self) -> &str {
        self.body["action"].as_str().unwrap_or_default()
    }
}

/// What the handler answers.
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

impl Reply {
    pub fn ok(body: &str) -> Self {
        Self::status(200, body)
    }
    pub fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }
    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

type Handler = dyn Fn(&Received) -> Reply + Send + Sync;

pub struct FakeBrain {
    pub url: String,
    received: Arc<Mutex<Vec<Received>>>,
    max_in_flight: Arc<AtomicUsize>,
}

impl FakeBrain {
    pub async fn start(handler: impl Fn(&Received) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handler: Arc<Handler> = Arc::new(handler);
        let received = Arc::new(Mutex::new(Vec::new()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        {
            let (received, max_in_flight) = (received.clone(), max_in_flight.clone());
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(
                        stream,
                        handler.clone(),
                        received.clone(),
                        in_flight.clone(),
                        max_in_flight.clone(),
                    ));
                }
            });
        }
        Self {
            url,
            received,
            max_in_flight,
        }
    }

    /// Answers the recorded wire: the model directory, the selection and a
    /// decision.
    pub async fn recorded() -> Self {
        Self::start(recorded_reply).await
    }

    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    pub fn calls_of(&self, action: &str) -> usize {
        self.received()
            .iter()
            .filter(|r| r.action() == action)
            .count()
    }

    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

pub fn recorded_reply(received: &Received) -> Reply {
    match (received.model(), received.action()) {
        ("brain/decision", "models") => Reply::ok(MODELS),
        ("brain/decision", "select") => Reply::ok(SELECT),
        (_, "decide") => Reply::ok(DECIDE),
        _ => Reply::status(
            404,
            r#"{"error":{"message":"no such model","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#,
        ),
    }
}

/// A loopback address that accepts connections and never answers.
pub async fn silent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    url
}

/// A loopback address nothing listens on.
pub async fn closed() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

async fn serve(
    mut stream: tokio::net::TcpStream,
    handler: Arc<Handler>,
    received: Arc<Mutex<Vec<Received>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_len, content_length) = loop {
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (end + 4, length);
        }
    };
    while buf.len() < head_len + content_length {
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_len]).to_string();
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let authorization = head.lines().find_map(|l| {
        let lower = l.to_ascii_lowercase();
        lower
            .starts_with("authorization:")
            .then(|| l["authorization:".len()..].trim().to_string())
    });
    let body =
        serde_json::from_slice(&buf[head_len..head_len + content_length]).unwrap_or(Value::Null);
    let request = Received {
        path,
        authorization,
        body,
    };
    received.lock().unwrap().push(request.clone());

    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    max_in_flight.fetch_max(now, Ordering::SeqCst);
    let reply = handler(&request);
    tokio::time::sleep(reply.delay).await;
    in_flight.fetch_sub(1, Ordering::SeqCst);

    let response = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    let _ = stream.write_all(response.as_bytes()).await;
}
