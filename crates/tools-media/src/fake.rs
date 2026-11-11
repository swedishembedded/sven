// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for testing generative-media
// pipelines end to end without a GPU for its clients. If your team needs
// expertise in media pipeline verification then you can procure our services
// by sending an email to info@swedishembedded.com.
//! A fake OpenRouter-compatible video server for tests: it starts a job per
//! idempotency key, answers a repeated key with the same job, completes every
//! job on its second poll, and can fail the first jobs it starts. It also
//! plays the sven service's invocation API: a model list, invocations that
//! answer files by handle, and those files' bytes. Behind the
//! `fake-server` feature so the binary's tests drive the same server the
//! tool's tests do.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::mp4::fixture::Clip;

/// What the fake server was asked to start: idempotency key and body.
pub type Created = Arc<Mutex<Vec<(String, Value)>>>;

/// One invocation the fake server ran.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub key: String,
    /// The conversation it named (`X-Sven-Session`).
    pub session: Option<String>,
    pub body: Value,
}

pub struct Fake {
    /// The `/v1` base URL to give a client.
    pub base_url: String,
    /// The jobs started so far, in order: idempotency key and request body.
    pub created: Created,
    /// The invocations run so far, in order.
    pub invoked: Arc<Mutex<Vec<Invocation>>>,
}

/// The smallest valid PNG: one transparent pixel.
pub const ONE_PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// The handle of the fake's `index`th file.
#[must_use]
pub fn handle_of(index: usize) -> String {
    format!("mcp-file://files/00000000-0000-4000-8000-{index:012}")
}

/// What an invocation answers, by its operation.
fn invoked_answer(index: usize, body: &Value) -> Value {
    match body["operation"].as_str() {
        Some("chat") => {
            let text = format!("{} sees a red car", body["model"].as_str().unwrap_or("?"));
            // `#cutoff` in the prompt: the model ran into its output limit.
            let finish = if body["prompt"].as_str().unwrap_or("").contains("#cutoff") {
                "length"
            } else {
                "stop"
            };
            if body["delivery"]["mode"] == "artifact" {
                let mime = body["delivery"]["mimeType"]
                    .as_str()
                    .unwrap_or("text/markdown");
                let ext = if mime == "application/json" {
                    "json"
                } else if mime == "text/plain" {
                    "txt"
                } else {
                    "md"
                };
                let stem = body["delivery"]["name"].as_str().unwrap_or("answer");
                json!({"id": format!("inv{index}"), "status": "completed", "text": null, "finish_reason": finish,
                    "outputs": [{"uri": handle_of(index), "name": format!("{stem}.{ext}"), "mimeType": mime, "size": text.len()}], "usage": {"cost_micro_usd": "300"}})
            } else {
                json!({"id": format!("inv{index}"), "status": "completed", "text": text, "finish_reason": finish, "outputs": [], "usage": {"cost_micro_usd": "300"}})
            }
        }
        Some("speech") => json!({"id": format!("inv{index}"), "status": "completed", "text": null,
            "outputs": [{"uri": handle_of(index), "name": "audio-1.wav", "mimeType": "audio/wav", "size": 48}], "usage": {"cost_micro_usd": "250"}}),
        _ => json!({"id": format!("inv{index}"), "status": "completed", "text": null,
            "outputs": [{"uri": handle_of(index), "name": "image-1.png", "mimeType": "image/png", "size": ONE_PIXEL_PNG.len()}], "usage": {"cost_micro_usd": "41000"}}),
    }
}

/// A video server that fails the first `fail_first` jobs it starts, answers a
/// repeated key with the same job, and completes every job on its second poll.
pub async fn serve(fail_first: usize) -> Fake {
    serve_clips(fail_first, mock_clip).await
}

/// As [`serve`], with the bytes of every delivered clip made by `clip` from
/// the request that started its job.
pub async fn serve_clips(fail_first: usize, clip: fn(&Value) -> Vec<u8>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let created: Created = Arc::default();
    let invoked: Arc<Mutex<Vec<Invocation>>> = Arc::default();
    let jobs: Arc<Mutex<Vec<(String, u32)>>> = Arc::default();
    let seen = Arc::clone(&created);
    let ran = Arc::clone(&invoked);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (seen, jobs, ran) = (Arc::clone(&seen), Arc::clone(&jobs), Arc::clone(&ran));
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let mut words = line.split_whitespace();
                let (method, path) = (
                    words.next().unwrap_or("").to_owned(),
                    words.next().unwrap_or("").to_owned(),
                );
                let (mut length, mut key, mut session) = (0usize, String::new(), None::<String>);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).await.unwrap();
                    let header = header.trim();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = header.split_once(": ") {
                        match k.to_ascii_lowercase().as_str() {
                            "content-length" => length = v.parse().unwrap_or(0),
                            "idempotency-key" => key = v.to_owned(),
                            "x-sven-session" => session = Some(v.to_owned()),
                            _ => {}
                        }
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).await.unwrap();
                let (status, kind, reply): (u16, &str, Vec<u8>) = if method == "POST"
                    && path == "/v1/videos"
                {
                    let mut jobs = jobs.lock().unwrap();
                    let index = match jobs.iter().position(|(k, _)| *k == key) {
                        Some(index) => index,
                        None => {
                            let mut request: Value = serde_json::from_slice(&body).unwrap();
                            // The conversation it was asked for, as the server saw it.
                            if let Some(session) = &session {
                                request["_session"] = Value::String(session.clone());
                            }
                            seen.lock().unwrap().push((key.clone(), request));
                            jobs.push((key.clone(), 0));
                            jobs.len() - 1
                        }
                    };
                    (
                        202,
                        "application/json",
                        json!({"id": format!("job{index}"), "status": "pending"})
                            .to_string()
                            .into_bytes(),
                    )
                } else if method == "GET" && path.split('?').next() == Some("/v1/models") {
                    let models = json!({"data": [
                        {"id": "google/gemini-nano-banana-2.1", "name": "Nano Banana 2.1", "architecture": {"input_modalities": ["image", "text"], "output_modalities": ["image", "text"]},
                         "pricing": {"image": "0.04"}, "supported_parameters": {"aspect_ratio": {"type": "enum", "values": ["1:1", "16:9"]}, "input_references": {"type": "range", "min": 0, "max": 14}}},
                        {"id": "openai/tts", "architecture": {"input_modalities": ["text"], "output_modalities": ["speech"]}, "supported_voices": ["alloy", "nova"], "pricing": {"prompt": "0.000015"}},
                        {"id": "vision/a", "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}, "pricing": {"prompt": "0.000001", "completion": "0.000002"}}
                    ]});
                    (200, "application/json", models.to_string().into_bytes())
                } else if method == "POST" && path == "/v1/images" {
                    // OpenRouter's Image API, for a provider of its own.
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    ran.lock().unwrap().push(Invocation {
                        key: "direct".into(),
                        session: None,
                        body,
                    });
                    let image = base64::engine::general_purpose::STANDARD.encode(ONE_PIXEL_PNG);
                    (
                        200,
                        "application/json",
                        json!({"data": [{"b64_json": image, "media_type": "image/png"}]})
                            .to_string()
                            .into_bytes(),
                    )
                } else if method == "POST" && path == "/v1/audio/speech" {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let pcm = body["response_format"] == "pcm";
                    ran.lock().unwrap().push(Invocation {
                        key: "direct".into(),
                        session: None,
                        body,
                    });
                    if pcm {
                        (200, "audio/pcm", vec![0u8; 960])
                    } else {
                        (200, "audio/mpeg", b"ID3\x04\x00\x00mock".to_vec())
                    }
                } else if method == "POST" && path == "/v1/chat/completions" {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let answer = json!({"choices": [{"message": {"role": "assistant", "content": format!("{} sees a red car", body["model"].as_str().unwrap_or("?"))}}]});
                    ran.lock().unwrap().push(Invocation {
                        key: "direct".into(),
                        session: None,
                        body,
                    });
                    (200, "application/json", answer.to_string().into_bytes())
                } else if method == "POST" && path == "/v1/invocations" {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let mut ran = ran.lock().unwrap();
                    // The same key is the same invocation: answered again, run once.
                    let index = ran.iter().position(|i| i.key == key).unwrap_or_else(|| {
                        ran.push(Invocation {
                            key: key.clone(),
                            session: session.clone(),
                            body: body.clone(),
                        });
                        ran.len() - 1
                    });
                    (
                        200,
                        "application/json",
                        invoked_answer(index, &ran[index].body)
                            .to_string()
                            .into_bytes(),
                    )
                } else if method == "GET"
                    && path.starts_with("/v1/files/")
                    && path.ends_with("/content")
                {
                    if path.contains("4000-8000") {
                        (200, "image/png", ONE_PIXEL_PNG.to_vec())
                    } else {
                        (404, "application/json", b"{}".to_vec())
                    }
                } else if method == "GET" && path == "/v1/videos/models" {
                    let models = json!({"data": [{"id": "brain/mock-video", "supported_durations": [1, 2, 3], "supported_resolutions": ["480p"],
                        "generate_audio": false, "seed": true, "actions": [{"action": "extend", "edits": true}], "pricing_skus": {"duration_seconds": "0.01"}}]});
                    (200, "application/json", models.to_string().into_bytes())
                } else if let Some(rest) = path.strip_prefix("/v1/videos/job") {
                    let index: usize = rest.split(['/', '?']).next().unwrap().parse().unwrap();
                    if rest.contains("/content") {
                        let request = seen.lock().unwrap()[index].1.clone();
                        (200, "video/mp4", clip(&request))
                    } else {
                        let mut jobs = jobs.lock().unwrap();
                        jobs[index].1 += 1;
                        let answer = if jobs[index].1 < 2 {
                            json!({"id": format!("job{index}"), "status": "in_progress", "progress": {"step": 1, "total": 2}})
                        } else if index < fail_first {
                            json!({"id": format!("job{index}"), "status": "failed", "error": "the model ran out of memory"})
                        } else {
                            json!({"id": format!("job{index}"), "status": "completed", "usage": {"seconds": 1.125, "cost": 0.01}, "file_id": format!("file-{index}")})
                        };
                        (200, "application/json", answer.to_string().into_bytes())
                    }
                } else {
                    (404, "application/json", b"{}".to_vec())
                };
                let head = format!("HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len());
                let _ = write.write_all(head.as_bytes()).await;
                let _ = write.write_all(&reply).await;
            });
        }
    });
    Fake {
        base_url,
        created,
        invoked,
    }
}

/// The clip of [`serve`]: a few bytes that are not an MP4.
fn mock_clip(_request: &Value) -> Vec<u8> {
    b"\0\0\0\x18ftypisom-mock-clip".to_vec()
}

/// A whole MP4 with sound of the size and the frames the request asked for
/// (`size` as WIDTHxHEIGHT, `num_frames` among the action's params), for the
/// tests that probe what was delivered.
#[must_use]
pub fn rendered_clip(request: &Value) -> Vec<u8> {
    let (width, height) = request["size"]
        .as_str()
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((64, 48));
    let params = &request["provider"]["options"]["brain"]["params"];
    let frames = params["num_frames"].as_u64().unwrap_or(24);
    Clip::new(0, usize::try_from(frames).unwrap_or(24))
        .with_size(width, height)
        .with_sound(1)
        .to_bytes()
}

/// [`serve_clips`] on a thread of its own, for tests that are not async: the
/// server lives until the process ends.
///
/// # Panics
/// When the server cannot start.
#[must_use]
pub fn serve_in_background(fail_first: usize, clip: fn(&Value) -> Vec<u8>) -> Fake {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the fake video server");
        runtime.block_on(async move {
            let fake = serve_clips(fail_first, clip).await;
            let _ = sender.send(fake);
            std::future::pending::<()>().await;
        });
    });
    receiver.recv().expect("the fake video server starts")
}
