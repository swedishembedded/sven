// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The media tools against a fake OpenRouter-compatible video server: a clip
//! is generated, polled and written; the same request never renders twice,
//! here or after a restart; a failed render is asked again under a new name;
//! an edit sends the provider's own file, or the clip inline; and every
//! argument the schema does not name is refused before anything is sent.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::*;

/// What the fake server was asked to start: idempotency key and body.
type Created = Arc<Mutex<Vec<(String, Value)>>>;

struct Fake {
    base_url: String,
    created: Created,
}

/// A video server that fails the first `fail_first` jobs it starts, answers a
/// repeated key with the same job, and completes every job on its second poll.
async fn serve(fail_first: usize) -> Fake {
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
                        (200, "video/mp4", b"\0\0\0\x18ftypisom-mock-clip".to_vec())
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

fn tool(fake: &Fake, workdir: &Path) -> RunModelTool {
    let endpoint = MediaEndpoint {
        base_url: fake.base_url.clone(),
        api_key: "k".into(),
    };
    RunModelTool::new(
        endpoint,
        MediaConfig {
            timeout_secs: 60,
            ..MediaConfig::default()
        },
        workdir.to_path_buf(),
    )
}

async fn run(tool: &RunModelTool, args: Value) -> ToolOutput {
    tool.execute(&ToolCall {
        id: "c1".into(),
        name: "run_model".into(),
        args,
    })
    .await
}

#[tokio::test]
async fn a_clip_is_written_and_the_same_request_never_renders_twice() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let args =
        json!({"model": "brain/mock-video", "prompt": "a red ball", "duration": 1, "seed": 3});
    let first = run(&tool(&fake, dir.path()), args.clone()).await;
    assert!(!first.is_error, "{}", first.content);
    assert!(
        first
            .content
            .contains("Saved the video to .sven/media/sven-"),
        "{}",
        first.content
    );
    assert!(first.content.contains("1.1 s"), "{}", first.content);
    let saved = std::fs::read_dir(dir.path().join(".sven/media"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "mp4"))
        .count();
    assert_eq!(saved, 1);
    // Asked again, by a new tool as after a restart: the file answers, nothing is sent.
    let again = run(&tool(&fake, dir.path()), args).await;
    assert!(again.content.contains("made earlier"), "{}", again.content);
    assert_eq!(fake.created.lock().unwrap().len(), 1);
    // Another seed is another take.
    run(
        &tool(&fake, dir.path()),
        json!({"model": "brain/mock-video", "prompt": "a red ball", "duration": 1, "seed": 4}),
    )
    .await;
    assert_eq!(fake.created.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_failed_render_is_asked_again_under_a_new_name() {
    let fake = serve(1).await;
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        &tool(&fake, dir.path()),
        json!({"model": "brain/mock-video", "prompt": "x"}),
    )
    .await;
    assert!(!out.is_error, "{}", out.content);
    let created = fake.created.lock().unwrap();
    assert_eq!(created.len(), 2);
    assert_ne!(created[0].0, created[1].0);
}

#[tokio::test]
async fn an_edit_names_the_providers_own_file_or_sends_the_clip_inline() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = tool(&fake, dir.path());
    let made = run(&t, json!({"model": "brain/mock-video", "prompt": "start"})).await;
    let clip = made
        .content
        .split_whitespace()
        .find(|w| w.starts_with(".sven/media/"))
        .unwrap()
        .to_owned();
    let out = run(&t, json!({"model": "brain/mock-video", "prompt": "more", "edit": "extend", "input_video": clip, "params": {"frames": 8}})).await;
    assert!(!out.is_error, "{}", out.content);
    let edit = fake.created.lock().unwrap()[1].1.clone();
    assert_eq!(
        edit["provider"]["options"]["brain"],
        json!({"action": "extend", "params": {"frames": 8}})
    );
    // The provider keeps the first clip: it is named, not uploaded again.
    assert_eq!(
        edit["input_video"]["video_url"]["url"],
        json!("file:file-0")
    );

    std::fs::write(dir.path().join("own.mp4"), b"\0\0\0\x18ftypisom").unwrap();
    run(&t, json!({"model": "brain/mock-video", "prompt": "more", "edit": "extend", "input_video": "own.mp4"})).await;
    let inline = fake.created.lock().unwrap()[2].1["input_video"]["video_url"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(inline.starts_with("data:video/mp4;base64,"), "{inline}");
}

#[tokio::test]
async fn models_lists_what_each_model_renders_and_costs() {
    let fake = serve(0).await;
    let out = ModelsTool::new(MediaEndpoint {
        base_url: fake.base_url.clone(),
        api_key: "k".into(),
    })
    .execute(&ToolCall {
        id: "c".into(),
        name: "models".into(),
        args: json!({}),
    })
    .await;
    assert_eq!(
        out.content,
        "brain/mock-video: text->video, 1-3 s, 480p, edits: extend, $0.01/s (duration_seconds)"
    );
}

#[test]
fn arguments_the_schema_does_not_name_are_refused_before_anything_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let refused = |args: Value| plan(&args, dir.path()).unwrap_err();
    assert!(refused(json!({"prompt": "x"})).contains("model"));
    assert!(refused(json!({"model": "m"})).contains("prompt"));
    assert!(refused(json!({"model": "m", "prompt": "x", "colour": "red"})).contains("colour"));
    assert!(
        refused(json!({"model": "m", "prompt": "x", "edit": "extend"})).contains("input_video")
    );
    assert!(refused(json!({"model": "m", "prompt": "x", "params": {}})).contains("edit"));
    assert!(refused(
        json!({"model": "m", "prompt": "x", "edit": "extend", "input_video": "/etc/hostname"})
    )
    .contains("working directory"));
}

#[test]
fn equal_requests_have_one_name_and_any_difference_another() {
    let dir = tempfile::tempdir().unwrap();
    let p = |args: Value| plan(&args, dir.path()).unwrap();
    let a = p(json!({"model": "m", "prompt": "x", "seed": 1, "duration": 2}));
    let b = p(json!({"duration": 2, "seed": 1, "prompt": "x", "model": "m"}));
    assert_eq!(request_key("u", &a, None, 0), request_key("u", &b, None, 0));
    assert_ne!(request_key("u", &a, None, 0), request_key("u", &a, None, 1));
    assert_ne!(request_key("u", &a, None, 0), request_key("v", &a, None, 0));
    assert_ne!(
        request_key("u", &a, None, 0),
        request_key("u", &a, Some("d"), 0)
    );
    assert_ne!(
        request_key("u", &a, None, 0),
        request_key(
            "u",
            &p(json!({"model": "m", "prompt": "x", "seed": 2, "duration": 2})),
            None,
            0
        )
    );
}
