// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `models`, `model_schema` and `run_model` for every modality but video,
//! against the fake sven service and a provider of its own: what a model
//! makes is a handle in a conversation and a file elsewhere, the same call is
//! one invocation, and a chat answer kept as a file never reaches the
//! session's model as text.

use super::*;
use std::sync::Arc;

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
async fn models_answers_what_kinds_there_are_before_it_lists_any() {
    let fake = serve(0).await;
    let models = ModelsTool::new(endpoint(&fake));
    let overview = call(&models, "m", json!({})).await.content;
    for kind in [
        "- text (chat, and vision: reading images): 2",
        "- image (pictures): 1",
        "- speech (text to speech, voices): 1",
        "- video (video clips): 3",
    ] {
        assert!(overview.contains(kind), "{kind} in {overview}");
    }
    // What is not here is said, so nothing is searched for.
    assert!(overview.contains("Nothing here makes audio"), "{overview}");
    assert!(overview.contains(r#"{"output": "image"}"#), "{overview}");
}

#[tokio::test]
async fn models_says_plainly_when_nothing_makes_what_was_asked_and_to_ask_the_person() {
    let fake = serve(0).await;
    let models = ModelsTool::new(endpoint(&fake));
    for asked in ["music", "audio", "song"] {
        let answer = call(&models, "m", json!({"output": asked})).await.content;
        assert!(
            answer.contains("No model this session can run makes audio"),
            "{asked}: {answer}"
        );
        assert!(answer.contains("ask"), "{asked}: {answer}");
        assert!(!answer.contains("try fewer filters"), "{asked}: {answer}");
    }
    // A kind there is, but no model of it by that name: its models, not a dead end.
    let answer = call(
        &models,
        "m",
        json!({"output": "speech", "query": "elevenlabs"}),
    )
    .await
    .content;
    assert!(answer.contains("openai/tts"), "{answer}");
}

#[tokio::test]
async fn models_takes_the_everyday_word_for_a_kind() {
    let fake = serve(0).await;
    let models = ModelsTool::new(endpoint(&fake));
    for word in ["voice", "tts", "Speech"] {
        let answer = call(&models, "m", json!({"output": word})).await.content;
        assert!(answer.starts_with("openai/tts"), "{word}: {answer}");
    }
    let answer = call(&models, "m", json!({"output": "picture"}))
        .await
        .content;
    assert!(
        answer.starts_with("google/gemini-nano-banana-2.1"),
        "{answer}"
    );
}

#[test]
fn run_model_runs_what_was_asked_and_asks_before_running_more() {
    let tool = RunModelTool::new(
        MediaEndpoint {
            base_url: "http://x".into(),
            api_key: "k".into(),
        },
        MediaConfig::default(),
        std::env::temp_dir(),
    );
    let said = tool.description();
    assert!(!said.contains("at once"), "{said}");
    assert!(said.contains("ask the person first"), "{said}");
    // Nothing invites it to price what it ran.
    assert!(
        !said.contains("cost;") && !said.contains("what they cost"),
        "{said}"
    );
    let listed = ModelsTool::new(MediaEndpoint {
        base_url: "http://x".into(),
        api_key: "k".into(),
    });
    assert!(
        listed.description().contains("never add up or report"),
        "{}",
        listed.description()
    );
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
    // What it made, never what it cost: the person sees the charge beside
    // the conversation, and a figure here is only one the model would add up.
    assert!(made.content.contains(&handle), "{}", made.content);
    assert!(
        !made.content.contains('$') && !made.content.contains("Charged"),
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

#[tokio::test]
async fn a_chat_answer_kept_as_a_file_is_answered_by_handle_and_its_text_stays_out_of_the_session()
{
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let tool = invoking(&fake, dir.path(), Some("conv-1"));
    let page = handle_of(7);
    let read = call(&tool, "call_1", json!({"model": "vision/a", "operation": "chat", "prompt": "Transcribe this page to Markdown", "inputs": [page], "keep_as": "markdown", "name": "page-3"})).await;
    assert!(!read.is_error, "{}", read.content);
    let ran = fake.invoked.lock().unwrap().clone();
    assert_eq!(
        ran[0].body["delivery"],
        json!({"mode": "artifact", "mimeType": "text/markdown", "name": "page-3"})
    );
    assert_eq!(read.artifacts.len(), 1);
    assert_eq!(read.artifacts[0].mime_type, "text/markdown");
    assert!(
        read.content.contains(&read.artifacts[0].uri),
        "{}",
        read.content
    );
    assert!(
        !read.content.contains("sees a red car"),
        "the text is not answered: {}",
        read.content
    );
    assert!(!read
        .parts
        .iter()
        .any(|p| matches!(p, ToolOutputPart::Image(_))));

    // An answer cut off at the model's output limit says so.
    let cut = call(&tool, "call_2", json!({"model": "vision/a", "operation": "chat", "prompt": "#cutoff a dense page", "keep_as": "markdown"})).await;
    assert!(cut.content.contains("cut off"), "{}", cut.content);

    // Only a chat answer is kept this way, and only as one of three kinds.
    for (args, why) in [
        (
            json!({"model": "m", "operation": "image", "prompt": "x", "keep_as": "markdown"}),
            "chat",
        ),
        (
            json!({"model": "m", "operation": "chat", "prompt": "x", "keep_as": "pdf"}),
            "markdown, json or text",
        ),
    ] {
        let refused = call(&tool, "call_x", args).await;
        assert!(
            refused.is_error && refused.content.contains(why),
            "{}",
            refused.content
        );
    }
    assert_eq!(
        fake.invoked.lock().unwrap().len(),
        2,
        "nothing refused is sent"
    );
}

#[tokio::test]
async fn a_provider_of_its_own_keeps_a_chat_answer_as_a_file_here() {
    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let tool = RunModelTool::new(
        endpoint(&fake),
        MediaConfig {
            provider: Some("openrouter".into()),
            ..MediaConfig::default()
        },
        dir.path().to_path_buf(),
    );
    let read = call(&tool, "c1", json!({"model": "vision/a", "operation": "chat", "prompt": "Extract every field as JSON", "keep_as": "text", "output": "out/fields.txt"})).await;
    assert!(!read.is_error, "{}", read.content);
    assert_eq!(read.artifacts[0].uri, "out/fields.txt");
    assert_eq!(read.artifacts[0].mime_type, "text/plain");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out/fields.txt")).unwrap(),
        "vision/a sees a red car"
    );
    assert!(!read.content.contains("sees a red car"), "{}", read.content);
}

/// Answers every question the same way, counting them.
struct Answers(SharingAnswer, std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl SharingConsent for Answers {
    async fn ask(&self, _model: &str) -> SharingAnswer {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.0
    }
}

/// Spec: a model the person has not allowed to keep data is asked about
/// where someone answers, recorded as they answer and run again; with nobody
/// to ask, the refusal is the answer, naming its code, and nothing waits.
#[tokio::test]
async fn a_model_that_may_keep_data_is_asked_about_then_run_or_refused() {
    let args = json!({"model": "keeps/video", "operation": "image", "prompt": "a fox"});

    let fake = serve(0).await;
    let dir = tempfile::tempdir().unwrap();
    let headless = invoking(&fake, dir.path(), None);
    let refused = call(&headless, "h1", args.clone()).await;
    assert!(refused.is_error);
    assert!(
        refused.content.contains("data_sharing_not_allowed"),
        "{}",
        refused.content
    );
    assert!(
        fake.allowed.lock().unwrap().is_empty(),
        "nothing is allowed for the person"
    );

    // Allowed once, the grant is spent by the run it allowed.
    for (answer, recorded) in [
        (SharingAnswer::Once, "spent"),
        (SharingAnswer::Always, "always"),
    ] {
        let fake = serve(0).await;
        let asked = Arc::new(Answers(answer, Default::default()));
        let tool = invoking(&fake, dir.path(), None).with_consent(Some(asked.clone()));
        let ran = call(&tool, "c1", args.clone()).await;
        assert!(!ran.is_error, "{}", ran.content);
        assert_eq!(asked.1.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            fake.allowed.lock().unwrap()[0],
            ("keeps/video".to_owned(), recorded.to_owned())
        );
        assert_eq!(
            fake.invoked.lock().unwrap().len(),
            1,
            "run once, after it was allowed"
        );
    }

    let fake = serve(0).await;
    let tool = invoking(&fake, dir.path(), None).with_consent(Some(Arc::new(Answers(
        SharingAnswer::Deny,
        Default::default(),
    ))));
    let denied = call(&tool, "d1", args).await;
    assert!(denied.is_error);
    assert!(
        denied.content.contains("did not allow"),
        "{}",
        denied.content
    );
    assert!(fake.allowed.lock().unwrap().is_empty());
    assert!(fake.invoked.lock().unwrap().is_empty());

    // A model that keeps no data is never asked about.
    let fake = serve(0).await;
    let asked = Arc::new(Answers(SharingAnswer::Deny, Default::default()));
    let tool = invoking(&fake, dir.path(), None).with_consent(Some(asked.clone()));
    assert!(!call(&tool, "n1", json!({"model": "google/gemini-nano-banana-2.1", "operation": "image", "prompt": "a fox"})).await.is_error);
    assert_eq!(asked.1.load(std::sync::atomic::Ordering::SeqCst), 0);
}
