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
//! job on its second poll, and can fail the first jobs it starts. Behind the
//! `fake-server` feature so the binary's tests drive the same server the
//! tool's tests do.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::mp4::fixture::Clip;

/// What the fake server was asked to start: idempotency key and body.
pub type Created = Arc<Mutex<Vec<(String, Value)>>>;

pub struct Fake {
    /// The `/v1` base URL to give a client.
    pub base_url: String,
    /// The jobs started so far, in order: idempotency key and request body.
    pub created: Created,
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
    let jobs: Arc<Mutex<Vec<(String, u32)>>> = Arc::default();
    let seen = Arc::clone(&created);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (seen, jobs) = (Arc::clone(&seen), Arc::clone(&jobs));
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
                let (mut length, mut key) = (0usize, String::new());
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
                            seen.lock()
                                .unwrap()
                                .push((key.clone(), serde_json::from_slice(&body).unwrap()));
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
    Fake { base_url, created }
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
