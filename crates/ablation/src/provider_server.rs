// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Serves any [`DecisionProvider`] over the wire a Brain server speaks
//! (`POST /v1/run`), so a sven process reaches the benchmark's providers
//! through the same `decision` configuration it uses for a real model. No
//! product code knows the difference.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sven_decision::{
    Answer, Choice, ChoiceOption, DecisionProvider, DecisionRequest, Probability, Question,
    QuestionKind, Score,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The model family that lists models and says which `auto` means.
const DIRECTORY_MODEL: &str = "brain/decision";
/// Largest request body the server reads.
const MAX_BODY: usize = 4 << 20;
/// The input limit the served model reports.
const MAX_INPUT_TOKENS: u64 = 4096;

/// What the server has done.
#[derive(Debug, Default)]
pub struct ServedStats {
    decisions: AtomicU64,
    questions: AtomicU64,
    micros: AtomicU64,
}

impl ServedStats {
    /// `decide` requests answered.
    #[must_use]
    pub fn decisions(&self) -> u64 {
        self.decisions.load(Ordering::Relaxed)
    }

    /// Questions answered.
    #[must_use]
    pub fn questions(&self) -> u64 {
        self.questions.load(Ordering::Relaxed)
    }

    /// Time the provider spent answering, in milliseconds.
    #[must_use]
    pub fn provider_ms(&self) -> f64 {
        self.micros.load(Ordering::Relaxed) as f64 / 1000.0
    }
}

/// One question as it was asked, and what the provider put first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServedQuestion {
    pub id: String,
    pub kind: String,
    pub instruction: String,
    /// `(id, description)` of each option, in the order sent.
    pub options: Vec<(String, Option<String>)>,
    pub top: Option<String>,
    pub top_probability: Option<f32>,
}

/// One `decide` request answered: enough to audit a decision later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServedDecision {
    /// The digest sven records in its trajectory for the same inputs.
    pub inputs_digest: String,
    pub state: String,
    pub questions: Vec<ServedQuestion>,
}

/// A running server on a loopback port.
pub struct ProviderServer {
    addr: SocketAddr,
    stats: Arc<ServedStats>,
    log: Arc<Mutex<Vec<ServedDecision>>>,
    task: JoinHandle<()>,
}

impl ProviderServer {
    /// Starts serving `provider` as the model `model_id`.
    ///
    /// # Errors
    ///
    /// The listener could not be bound.
    pub async fn start(
        provider: Arc<dyn DecisionProvider>,
        model_id: &str,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let stats = Arc::new(ServedStats::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(Inner {
            provider,
            model_id: model_id.to_string(),
            stats: Arc::clone(&stats),
            log: Arc::clone(&log),
        });
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(connection(stream, Arc::clone(&shared)));
            }
        });
        Ok(Self {
            addr,
            stats,
            log,
            task,
        })
    }

    /// The base URL to put in `decision.brain.url`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    #[must_use]
    pub fn stats(&self) -> &Arc<ServedStats> {
        &self.stats
    }

    /// Every `decide` request answered so far, in order.
    #[must_use]
    pub fn served(&self) -> Vec<ServedDecision> {
        self.log
            .lock()
            .expect("the log lock is never poisoned")
            .clone()
    }
}

impl Drop for ProviderServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Inner {
    provider: Arc<dyn DecisionProvider>,
    model_id: String,
    stats: Arc<ServedStats>,
    log: Arc<Mutex<Vec<ServedDecision>>>,
}

type Shared = Arc<Inner>;

async fn connection(mut stream: TcpStream, shared: Shared) {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let Some((head_end, content_length)) = read_head(&mut stream, &mut buffer).await else {
            return;
        };
        if content_length > MAX_BODY {
            let _ = respond(
                &mut stream,
                413,
                &json!({"error": {"code": "invalid_request", "message": "body too large"}}),
            )
            .await;
            return;
        }
        while buffer.len() < head_end + content_length {
            let mut chunk = [0_u8; 8192];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            }
        }
        let body: Vec<u8> = buffer[head_end..head_end + content_length].to_vec();
        buffer.drain(..head_end + content_length);
        let (status, reply) = handle(&shared, &body).await;
        if respond(&mut stream, status, &reply).await.is_err() {
            return;
        }
    }
}

/// Reads until the end of the headers; the offset after them and the declared
/// body length. `None` when the peer closed or sent nonsense.
async fn read_head(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<(usize, usize)> {
    loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..at]).to_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(Some(0), |v| v.trim().parse().ok())?;
            return Some((at + 4, length));
        }
        let mut chunk = [0_u8; 8192];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
        if buffer.len() > MAX_BODY {
            return None;
        }
    }
}

async fn respond(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let text = body.to_string();
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        text.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(text.as_bytes()).await
}

fn failure(code: &str, message: &str) -> (u16, Value) {
    (500, json!({"error": {"code": code, "message": message}}))
}

async fn handle(shared: &Shared, body: &[u8]) -> (u16, Value) {
    let Ok(call) = serde_json::from_slice::<Value>(body) else {
        return (
            422,
            json!({"error": {"code": "invalid_request", "message": "not JSON"}}),
        );
    };
    let Inner {
        provider,
        model_id,
        stats,
        log,
    } = &**shared;
    let model = call.get("model").and_then(Value::as_str).unwrap_or("");
    let action = call.get("action").and_then(Value::as_str).unwrap_or("");
    let ok = |outputs: Value| (200, json!({"outputs": outputs, "blobs": {}}));
    match (model, action) {
        (DIRECTORY_MODEL, "models") => ok(json!({"models": [model_record(model_id)]})),
        (DIRECTORY_MODEL, "select") => ok(json!({
            "chosen": model_id,
            "rule": "ablation",
            "reason": format!("{model_id} is the only model this benchmark serves"),
        })),
        (m, "decide") if m == model_id => {
            let request = match decode_request(call.get("params").unwrap_or(&Value::Null)) {
                Ok(r) => r,
                Err(e) => {
                    return (
                        422,
                        json!({"error": {"code": "invalid_request", "message": e}}),
                    )
                }
            };
            let started = Instant::now();
            let result = provider.decide(&request).await;
            let elapsed = started.elapsed();
            stats.decisions.fetch_add(1, Ordering::Relaxed);
            stats
                .questions
                .fetch_add(request.questions.len() as u64, Ordering::Relaxed);
            stats
                .micros
                .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
            match result {
                Ok(result) => {
                    log.lock()
                        .expect("the log lock is never poisoned")
                        .push(served_decision(&request, &result));
                    ok(encode_result(model_id, &request, &result, elapsed))
                }
                Err(e) => failure("backend_failure", &e.to_string()),
            }
        }
        _ => (
            404,
            json!({"error": {"code": "model_not_found", "message": format!("no model {model:?}")}}),
        ),
    }
}

fn model_record(model_id: &str) -> Value {
    let checkpoint = hex::encode(Sha256::digest(model_id.as_bytes()));
    json!({
        "id": model_id, "state": "ready", "configured": true, "backend": "ablation",
        "validated": true,
        "manifest": {
            "schema": 1, "capability": "decide", "architecture": "ablation",
            "weights_sha256": format!("sha256:{checkpoint}"),
            "max_input_tokens": MAX_INPUT_TOKENS,
            "question_types": ["choice", "score", "noul"],
            "validation": {"status": "validated", "profile": "ablation"},
        },
    })
}

fn decode_request(params: &Value) -> Result<DecisionRequest, String> {
    let state = params
        .get("state")
        .and_then(Value::as_str)
        .ok_or("params.state is missing")?
        .to_string();
    // The questions travel as a JSON document inside a string; accept the
    // array itself as well.
    let questions: Value = match params.get("questions") {
        Some(Value::String(text)) => serde_json::from_str(text).map_err(|e| e.to_string())?,
        Some(other) => other.clone(),
        None => return Err("params.questions is missing".into()),
    };
    let deadline = params
        .get("deadline_ms")
        .and_then(Value::as_u64)
        .map_or(Duration::from_millis(500), Duration::from_millis);
    let questions = questions
        .as_array()
        .ok_or("params.questions is not an array")?
        .iter()
        .map(decode_question)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DecisionRequest {
        state,
        questions,
        deadline,
    })
}

fn decode_question(q: &Value) -> Result<Question, String> {
    let text = |key: &str| q.get(key).and_then(Value::as_str).map(str::to_string);
    let id = text("id").ok_or("a question has no id")?;
    let instruction = text("instruction").unwrap_or_default();
    match q.get("type").and_then(Value::as_str) {
        Some("choice") => Ok(Question::Choice(Choice {
            id,
            instruction,
            options: q
                .get("options")
                .and_then(Value::as_array)
                .ok_or("a choice has no options")?
                .iter()
                .map(|o| ChoiceOption {
                    id: o
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    description: o
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect(),
        })),
        Some("noul") => Ok(Question::Probability(Probability {
            id,
            proposition: instruction,
        })),
        Some("score") => Ok(Question::Score(Score {
            id,
            instruction,
            levels: q
                .get("options")
                .and_then(Value::as_array)
                .ok_or("a score has no levels")?
                .iter()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect(),
        })),
        other => Err(format!("unknown question type {other:?}")),
    }
}

fn encode_result(
    model_id: &str,
    request: &DecisionRequest,
    result: &sven_decision::DecisionResult,
    elapsed: Duration,
) -> Value {
    let tokens = (request.state.len() / 4 + 1) as u64;
    let answers: Vec<Value> = result
        .answers
        .iter()
        .map(|a| match &a.answer {
            Answer::Ranked(d) => {
                let mut v = json!({
                    "id": a.id,
                    "type": match a.kind {
                        QuestionKind::Choice => "choice",
                        QuestionKind::Score => "score",
                        QuestionKind::Probability => "noul",
                    },
                    "ranked": d.ranked.iter().map(|c| json!({
                        "id": c.id, "index": c.index, "probability": c.probability,
                    })).collect::<Vec<_>>(),
                    "state": {"tokens": tokens, "kept": tokens, "truncated": false},
                });
                if let Some(confidence) = d.confidence {
                    v["confidence"] = json!(confidence);
                }
                v
            }
            Answer::Unsupported { reason } => {
                json!({"id": a.id, "unsupported": true, "reason": reason})
            }
        })
        .collect();
    json!({
        "model": model_id,
        "latency_ms": elapsed.as_secs_f64() * 1000.0,
        "max_input_tokens": MAX_INPUT_TOKENS,
        "flags": {"state_truncated": false, "unsupported_question_types": []},
        "answers": answers,
    })
}

fn served_decision(
    request: &DecisionRequest,
    result: &sven_decision::DecisionResult,
) -> ServedDecision {
    let questions = request
        .questions
        .iter()
        .map(|q| {
            let (kind, instruction, options) = match q {
                Question::Choice(c) => (
                    "choice",
                    c.instruction.clone(),
                    c.options
                        .iter()
                        .map(|o| (o.id.clone(), o.description.clone()))
                        .collect(),
                ),
                Question::Probability(p) => ("noul", p.proposition.clone(), Vec::new()),
                Question::Score(s) => (
                    "score",
                    s.instruction.clone(),
                    s.levels.iter().map(|l| (l.clone(), None)).collect(),
                ),
            };
            let top = result.answer(q.id()).and_then(|a| match &a.answer {
                Answer::Ranked(d) => Some((d.top().id.clone(), d.top().probability)),
                Answer::Unsupported { .. } => None,
            });
            ServedQuestion {
                id: q.id().to_string(),
                kind: kind.to_string(),
                instruction,
                options,
                top: top.as_ref().map(|t| t.0.clone()),
                top_probability: top.map(|t| t.1),
            }
        })
        .collect();
    ServedDecision {
        inputs_digest: sven_decision::telemetry::inputs_digest(request),
        state: request.state.clone(),
        questions,
    }
}
