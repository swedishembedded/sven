// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The media tools against a fake OpenRouter-compatible video server: a clip
//! is generated, polled and written; the same request never renders twice,
//! here or after a restart; a failed render is asked again under a new name;
//! an edit sends the provider's own file, or the clip inline; and every
//! argument the schema does not name is refused before anything is sent.

use super::*;
use base64::Engine as _;
use serde_json::{json, Value};
use std::time::Duration;

use crate::fake::{serve, Fake};
use crate::request::{plan, request_key};

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

/// A working directory holding one file per reference kind.
fn with_references() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("face.png"), b"png-bytes").unwrap();
    std::fs::write(dir.path().join("pose.jpg"), b"jpg-bytes").unwrap();
    std::fs::write(dir.path().join("move.mp4"), b"mp4-bytes").unwrap();
    std::fs::write(dir.path().join("voice.wav"), b"wav-bytes").unwrap();
    dir
}

fn reference_request() -> Value {
    json!({
        "model": "brain/mock-video", "prompt": "<Picture 1> speaks <Audio 1>",
        "action": "ref2va", "size": "832x480", "params": {"steps": 4},
        "references": [
            {"path": "face.png", "kind": "image"},
            {"path": "move.mp4", "kind": "video"},
            {"path": "voice.wav", "kind": "audio"},
            {"path": "pose.jpg", "kind": "image"},
        ],
    })
}

#[tokio::test]
async fn references_an_action_and_an_exact_size_reach_the_server_in_order() {
    let fake = serve(0).await;
    let dir = with_references();
    let out = run(&tool(&fake, dir.path()), reference_request()).await;
    assert!(!out.is_error, "{}", out.content);
    let body = fake.created.lock().unwrap()[0].1.clone();
    assert_eq!(body["size"], "832x480");
    assert_eq!(
        body["provider"]["options"]["brain"],
        json!({"action": "ref2va", "params": {"steps": 4}})
    );
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    assert_eq!(
        body["input_references"],
        json!([
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", b64(b"png-bytes"))}},
            {"type": "video_url", "video_url": {"url": format!("data:video/mp4;base64,{}", b64(b"mp4-bytes"))}},
            {"type": "audio_url", "audio_url": {"url": format!("data:audio/wav;base64,{}", b64(b"wav-bytes"))}},
            {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", b64(b"jpg-bytes"))}},
        ])
    );
}

#[tokio::test]
async fn a_changed_reference_is_a_new_request_and_an_unchanged_one_is_not() {
    let fake = serve(0).await;
    let dir = with_references();
    let t = tool(&fake, dir.path());
    run(&t, reference_request()).await;
    run(&t, reference_request()).await;
    assert_eq!(fake.created.lock().unwrap().len(), 1);
    std::fs::write(dir.path().join("face.png"), b"another-face").unwrap();
    run(&t, reference_request()).await;
    assert_eq!(fake.created.lock().unwrap().len(), 2);
    // The same files in another order are another request: the prompt names them by order.
    let mut swapped = reference_request();
    swapped["references"].as_array_mut().unwrap().swap(0, 3);
    run(&t, swapped).await;
    assert_eq!(fake.created.lock().unwrap().len(), 3);
}

#[test]
fn references_the_working_directory_or_their_kind_cannot_vouch_for_are_refused() {
    let dir = with_references();
    let refused = |patch: Value| {
        let mut args = json!({"model": "m", "prompt": "x"});
        args.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        plan(&args, dir.path()).unwrap_err()
    };
    let one = |path: &str, kind: &str| json!({"references": [{"path": path, "kind": kind}]});
    assert!(refused(one("/etc/hostname", "image")).contains("working directory"));
    assert!(refused(one("face.png", "audio")).contains("audio"));
    assert!(refused(one("voice.wav", "video")).contains("video"));
    assert!(refused(one("face.png", "sprite")).contains("sprite"));
    assert!(refused(one("missing.png", "image")).contains("missing.png"));
    assert!(refused(json!({"references": [{"path": "face.png"}]})).contains("kind"));
    assert!(refused(json!({"references": []})).contains("references"));
    assert!(
        refused(json!({"action": "a", "edit": "extend", "input_video": "move.mp4"}))
            .contains("both")
    );
    assert!(refused(json!({"params": {}})).contains("action"));
    for size in ["832", "832x", "0x480", "-4x480", "axb", "832x480x2"] {
        assert!(refused(json!({"size": size})).contains("size"), "{size}");
    }
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
    assert_eq!(request_key("u", &a, &[], 0), request_key("u", &b, &[], 0));
    assert_ne!(request_key("u", &a, &[], 0), request_key("u", &a, &[], 1));
    assert_ne!(request_key("u", &a, &[], 0), request_key("v", &a, &[], 0));
    assert_ne!(
        request_key("u", &a, &[], 0),
        request_key("u", &a, &["video:d".to_owned()], 0)
    );
    assert_ne!(
        request_key("u", &a, &[], 0),
        request_key(
            "u",
            &p(json!({"model": "m", "prompt": "x", "seed": 2, "duration": 2})),
            &[],
            0
        )
    );
}

#[tokio::test]
async fn a_clip_is_also_saved_where_asked_and_never_outside_the_working_directory() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = tool(&fake, dir.path());
    let args = json!({"model": "brain/mock-video", "prompt": "a", "output": "film/shot-01.mp4"});
    let out = run(&t, args.clone()).await;
    assert!(
        out.content.contains("Saved the video to film/shot-01.mp4"),
        "{}",
        out.content
    );
    assert!(dir.path().join("film/shot-01.mp4").is_file());
    // The same request saved elsewhere is still one render.
    let again = run(
        &t,
        json!({"model": "brain/mock-video", "prompt": "a", "output": "film/take.mp4"}),
    )
    .await;
    assert!(again.content.contains("film/take.mp4"), "{}", again.content);
    assert_eq!(fake.created.lock().unwrap().len(), 1);
    for outside in ["../x.mp4", "/abs/x.mp4", "film/../../x.mp4", ""] {
        let refused = run(&t, json!({"model": "m", "prompt": "a", "output": outside})).await;
        assert!(
            refused.is_error && refused.content.contains("inside the working directory"),
            "{outside}: {}",
            refused.content
        );
    }
    assert_eq!(fake.created.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn join_videos_joins_clips_of_the_working_directory_in_order() {
    use crate::mp4::tests::Clip;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("shots")).unwrap();
    Clip::new(0, 24).write(&dir.path().join("shots/a.mp4"));
    Clip::new(1, 48).write(&dir.path().join("shots/b.mp4"));
    let tool = JoinVideosTool::new(dir.path().to_path_buf());
    let call = |args: Value| ToolCall {
        id: "j".into(),
        name: "join_videos".into(),
        args,
    };
    let out = tool
        .execute(&call(
            json!({"clips": ["shots/a.mp4", "shots/b.mp4"], "output": "film.mp4"}),
        ))
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(
        out.content,
        "Joined into film.mp4: 3.00 s, 72 frames, 64x48 av01."
    );
    let refused = tool
        .execute(&call(
            json!({"clips": ["/etc/hostname"], "output": "f.mp4"}),
        ))
        .await;
    assert!(
        refused.is_error && refused.content.contains("working directory"),
        "{}",
        refused.content
    );
    let refused = tool
        .execute(&call(
            json!({"clips": ["shots/a.mp4"], "output": "../f.mp4"}),
        ))
        .await;
    assert!(refused.is_error, "{}", refused.content);
}

fn endpoint(fake: &Fake) -> MediaEndpoint {
    MediaEndpoint {
        base_url: fake.base_url.clone(),
        api_key: "k".into(),
    }
}

#[tokio::test]
async fn a_generation_killed_while_polling_resumes_the_job_it_started() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let args = json!({"model": "brain/mock-video", "prompt": "a red ball", "seed": 3});
    let config = MediaConfig::default();
    let mut lines = Vec::new();
    // The process dies after submitting, while it waits for the job.
    let killed = tokio::time::timeout(
        Duration::from_millis(600),
        generate(&endpoint(&fake), &config, dir.path(), &args, &mut |line| {
            lines.push(line.to_owned())
        }),
    )
    .await;
    assert!(killed.is_err(), "the job takes two polls");
    assert!(lines.iter().any(|l| l.contains("job0")), "{lines:?}");
    assert!(!dir.path().join(".sven/media").exists());

    let done = generate(&endpoint(&fake), &config, dir.path(), &args, &mut |_| {})
        .await
        .unwrap();
    assert!(!done.reused);
    // The same idempotency key answered the job already running.
    assert_eq!(fake.created.lock().unwrap().len(), 1);
    assert_eq!(done.seconds, Some(1.125));
    assert_eq!(done.digest().unwrap().len(), 64);

    // Once written, the file answers: nothing is submitted, the sidecar's length is kept.
    let again = generate(&endpoint(&fake), &config, dir.path(), &args, &mut |_| {})
        .await
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.seconds, Some(1.125));
    assert_eq!(again.path, done.path);
    assert_eq!(fake.created.lock().unwrap().len(), 1);
}
