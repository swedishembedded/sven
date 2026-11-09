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

use crate::fake::{self as fixture, handle_of, serve, Fake};
use crate::request::{plan, request_key};
use sven_tool_api::tool::ToolOutputPart;

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

/// `run_model` asked for a video with `args`: the model, prompt and output
/// as they are, everything else the video API's own parameters.
async fn run(tool: &RunModelTool, args: Value) -> ToolOutput {
    let mut params = args.as_object().unwrap().clone();
    let mut call = serde_json::Map::new();
    call.insert("operation".into(), json!("video"));
    for key in ["model", "prompt", "output"] {
        if let Some(value) = params.remove(key) {
            call.insert(key.into(), value);
        }
    }
    if !params.is_empty() {
        call.insert("params".into(), Value::Object(params));
    }
    tool.execute(&ToolCall {
        id: "c1".into(),
        name: "run_model".into(),
        args: Value::Object(call),
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
        args: json!({"output": "video"}),
    })
    .await;
    assert_eq!(
        out.content,
        "brain/mock-video: text -> video; run as video; duration_seconds $0.01"
    );
    let schema = ModelSchemaTool::new(MediaEndpoint {
        base_url: fake.base_url.clone(),
        api_key: "k".into(),
    })
    .execute(&ToolCall {
        id: "c".into(),
        name: "model_schema".into(),
        args: json!({"model": "brain/mock-video"}),
    })
    .await;
    assert!(
        schema
            .content
            .contains("text->video, 1-3 s, 480p, edits: extend"),
        "{}",
        schema.content
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

// ------------------------------------------------------------------ every other modality

fn invoking(fake: &Fake, workdir: &Path, session: Option<&str>) -> RunModelTool {
    RunModelTool::new(
        endpoint(fake),
        MediaConfig {
            session: session.map(str::to_owned),
            ..MediaConfig::default()
        },
        workdir.to_path_buf(),
    )
}

async fn call(tool: &dyn Tool, id: &str, args: Value) -> ToolOutput {
    tool.execute(&ToolCall {
        id: id.into(),
        name: tool.name().into(),
        args,
    })
    .await
}

#[tokio::test]
async fn models_finds_a_model_by_what_it_makes_reads_and_is_called() {
    let fake = serve(0).await;
    let models = ModelsTool::new(endpoint(&fake));
    let speech = call(&models, "m", json!({"output": "speech"})).await;
    assert_eq!(
        speech.content,
        "openai/tts: text -> speech; run as speech; 2 voices; prompt $0.000015"
    );
    let vision = call(&models, "m", json!({"input": "image", "output": "text"})).await;
    assert!(
        vision.content.starts_with("google/gemini-nano-banana-2.1")
            && vision
                .content
                .contains("vision/a: text+image -> text; run as chat"),
        "{}",
        vision.content
    );
    let named = call(&models, "m", json!({"query": "nano banana"})).await;
    assert!(
        named
            .content
            .starts_with("google/gemini-nano-banana-2.1: image+text -> image+text; run as image"),
        "{}",
        named.content
    );
    let schema = call(
        &ModelSchemaTool::new(endpoint(&fake)),
        "s",
        json!({"model": "google/gemini-nano-banana-2.1"}),
    )
    .await;
    let schema: Value = serde_json::from_str(&schema.content).unwrap();
    assert_eq!(schema["operations"], json!(["image"]));
    assert_eq!(schema["parameters"]["input_references"]["max"], json!(14));
}

#[tokio::test]
async fn in_a_conversation_an_image_is_answered_by_handle_shown_to_the_model_and_edited_by_it() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let tool = invoking(&fake, dir.path(), Some("conv-1"));
    let made = call(&tool, "call_1", json!({"model": "google/gemini-nano-banana-2.1", "operation": "image", "prompt": "a red car", "params": {"aspect_ratio": "16:9"}})).await;
    assert!(!made.is_error, "{}", made.content);
    let handle = handle_of(0);
    assert_eq!(made.artifacts.len(), 1);
    assert_eq!(
        (
            made.artifacts[0].uri.as_str(),
            made.artifacts[0].mime_type.as_str()
        ),
        (handle.as_str(), "image/png")
    );
    assert!(
        made.content.contains(&handle) && made.content.contains("$0.0410"),
        "{}",
        made.content
    );
    // The session's model sees the picture; the bytes travel to it, not to the person.
    assert!(made.parts.iter().any(
        |p| matches!(p, ToolOutputPart::Image(url) if url.starts_with("data:image/png;base64,"))
    ));
    // Nothing was written here: the file is the conversation's.
    assert!(!dir.path().join(".sven").exists());

    let edited = call(&tool, "call_2", json!({"model": "google/gemini-nano-banana-2.1", "operation": "image", "prompt": "make it black, in snow", "inputs": [handle]})).await;
    assert!(!edited.is_error, "{}", edited.content);
    let ran = fake.invoked.lock().unwrap().clone();
    assert_eq!(ran.len(), 2);
    assert!(ran.iter().all(|i| i.session.as_deref() == Some("conv-1")));
    assert_eq!(ran[1].body["inputs"], json!([{"uri": handle}]));
    assert_eq!(ran[0].body["params"], json!({"aspect_ratio": "16:9"}));
}

#[tokio::test]
async fn the_same_call_asked_again_runs_once() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let tool = invoking(&fake, dir.path(), Some("conv-1"));
    let args = json!({"model": "openai/tts", "operation": "speech", "prompt": "my name is martin", "params": {"voice": "alloy"}});
    call(&tool, "call_1", args.clone()).await;
    let again = call(&tool, "call_1", args.clone()).await;
    assert_eq!(again.artifacts[0].mime_type, "audio/wav");
    // Speech is the person's to hear, never fed back to the model.
    assert!(!again
        .parts
        .iter()
        .any(|p| matches!(p, ToolOutputPart::Audio(_) | ToolOutputPart::Image(_))));
    assert_eq!(fake.invoked.lock().unwrap().len(), 1);
    call(&tool, "call_2", args).await;
    assert_eq!(fake.invoked.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn three_vision_models_read_one_picture_in_parallel() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let tool = invoking(&fake, dir.path(), Some("conv-1"));
    let picture = handle_of(7);
    let ask = |model: &str| json!({"model": model, "operation": "chat", "prompt": "Label the objects in this picture", "inputs": [picture.clone()]});
    let (a, b, c) = tokio::join!(
        call(&tool, "c1", ask("vision/a")),
        call(&tool, "c2", ask("vision/b")),
        call(&tool, "c3", ask("vision/c"))
    );
    for (out, model) in [(a, "vision/a"), (b, "vision/b"), (c, "vision/c")] {
        assert!(
            out.content.starts_with(&format!("{model} sees a red car")),
            "{}",
            out.content
        );
        assert!(out.artifacts.is_empty());
    }
    let ran = fake.invoked.lock().unwrap().clone();
    assert_eq!(ran.len(), 3);
    assert!(ran
        .iter()
        .all(|i| i.body["inputs"] == json!([{"uri": picture}])));
}

#[tokio::test]
async fn outside_a_conversation_what_a_model_made_is_saved_in_the_working_directory() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("car.png"), fixture::ONE_PIXEL_PNG).unwrap();
    let tool = invoking(&fake, dir.path(), None);
    let out = call(&tool, "call_1", json!({"model": "google/gemini-nano-banana-2.1", "operation": "image", "prompt": "in snow", "inputs": ["car.png"], "output": "art/snow.png"})).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.artifacts[0].uri, "art/snow.png");
    assert_eq!(
        std::fs::read(dir.path().join("art/snow.png")).unwrap(),
        fixture::ONE_PIXEL_PNG
    );
    let ran = fake.invoked.lock().unwrap().clone();
    assert_eq!(ran[0].session, None);
    assert_eq!(ran[0].body["inputs"][0]["mimeType"], json!("image/png"));
    let outside = call(
        &tool,
        "call_2",
        json!({"model": "m", "operation": "image", "prompt": "x", "inputs": ["/etc/hostname"]}),
    )
    .await;
    assert!(
        outside.is_error && outside.content.contains("working directory"),
        "{}",
        outside.content
    );
}

#[tokio::test]
async fn a_provider_of_its_own_is_called_directly_and_its_outputs_saved_here() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("car.png"), fixture::ONE_PIXEL_PNG).unwrap();
    let tool = RunModelTool::new(
        endpoint(&fake),
        MediaConfig {
            provider: Some("openrouter".into()),
            ..MediaConfig::default()
        },
        dir.path().to_path_buf(),
    );
    let image = call(&tool, "c1", json!({"model": "google/gemini-nano-banana-2.1", "operation": "image", "prompt": "in snow", "inputs": ["car.png"], "params": {"aspect_ratio": "16:9"}})).await;
    assert!(!image.is_error, "{}", image.content);
    assert_eq!(
        std::fs::read(dir.path().join(&image.artifacts[0].uri)).unwrap(),
        fixture::ONE_PIXEL_PNG
    );
    assert!(image
        .parts
        .iter()
        .any(|p| matches!(p, ToolOutputPart::Image(_))));
    let speech = call(&tool, "c2", json!({"model": "openai/tts", "operation": "speech", "prompt": "my name is martin", "params": {"voice": "alloy", "format": "pcm"}})).await;
    assert_eq!(speech.artifacts[0].mime_type, "audio/wav");
    let wav = std::fs::read(dir.path().join(&speech.artifacts[0].uri)).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    let label = call(&tool, "c3", json!({"model": "vision/a", "operation": "chat", "prompt": "label it", "inputs": ["car.png"]})).await;
    assert!(
        label.content.starts_with("vision/a sees a red car"),
        "{}",
        label.content
    );
    let ran = fake.invoked.lock().unwrap().clone();
    assert_eq!(ran[0].body["aspect_ratio"], json!("16:9"));
    assert!(ran[0].body["input_references"][0]["image_url"]["url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png;base64,"));
    assert_eq!(
        (ran[1].body["input"].as_str(), ran[1].body["voice"].as_str()),
        (Some("my name is martin"), Some("alloy"))
    );
    assert_eq!(
        ran[2].body["messages"][0]["content"][1]["type"],
        json!("image_url")
    );
    let handle = call(
        &tool,
        "c4",
        json!({"model": "m", "operation": "image", "prompt": "x", "inputs": [handle_of(1)]}),
    )
    .await;
    assert!(
        handle.is_error && handle.content.contains("by path"),
        "{}",
        handle.content
    );
}

#[tokio::test]
async fn a_video_for_a_conversation_is_its_file_too() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let tool = invoking(&fake, dir.path(), Some("conv-1"));
    let out = call(&tool, "c1", json!({"model": "brain/mock-video", "operation": "video", "prompt": "a red ball", "params": {"duration": 1}})).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.artifacts[0].uri, "mcp-file://files/file-0");
    assert_eq!(out.artifacts[0].mime_type, "video/mp4");
    assert!(
        out.content.contains("mcp-file://files/file-0"),
        "{}",
        out.content
    );
    // The job named the conversation; the clip is still here to be joined.
    assert_eq!(
        fake.created.lock().unwrap()[0].1["_session"],
        json!("conv-1")
    );
    assert!(
        dir.path().join(&out.artifacts[0].name).exists()
            || std::fs::read_dir(dir.path().join(".sven/media"))
                .unwrap()
                .count()
                > 0
    );
}
