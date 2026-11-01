// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a conversation's trajectory records what its model was actually
//! given: the user turn that carries a task's attachments (text and images),
//! the images a tool returned, and exactly the tools its requests offered.
//! Images are referenced by the digest of their bytes and stored once beside
//! the trajectory.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning multimodal agent runs
//! into reviewable training data then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde_json::json;
use sven_model::{ContentPart, MessageContent, ResponseEvent};
use sven_model_mock::CallbackProvider;
use sven_sdk::{ApprovalPolicy, Engine, RunOptions, Toolset};

const DOC: &str = r#"
api_version: "2"
name: traced
execution: { limits: { model_turns: 8, tool_calls: 8 } }
capabilities: [ReadFile, WriteFile]
workflow:
  - name: look
    tools: [read_file, write_file, attach_file]
    attach: [pic.png]
    task: Describe the picture.
  - name: plain
    tools: [read_file, write_file]
    task: Say done.
"#;

/// A 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Every image the model was given in any request, in order of first sight.
type Seen = Arc<Mutex<Vec<Vec<u8>>>>;

fn decode(url: &str) -> Vec<u8> {
    let (_, data) = sven_model::parse_data_url_parts(url).expect("a data URL");
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .expect("base64")
}

async fn executed() -> (sven_sdk::HarnessRun, Seen) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pic.png"), PNG).unwrap();
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    let provider = CallbackProvider::new(move |req| {
        let mut log = log.lock().unwrap();
        for message in &req.messages {
            let urls: Vec<&str> = match &message.content {
                MessageContent::ContentParts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Image { image_url, .. } => Some(image_url.as_str()),
                        _ => None,
                    })
                    .collect(),
                MessageContent::ToolResult { content, .. } => content.image_urls(),
                _ => Vec::new(),
            };
            for url in urls {
                let bytes = decode(url);
                if !log.contains(&bytes) {
                    log.push(bytes);
                }
            }
        }
        let answered = req
            .messages
            .iter()
            .any(|m| matches!(m.content, MessageContent::ToolResult { .. }));
        let first_turn = req
            .messages
            .iter()
            .any(|m| matches!(m.content, MessageContent::ContentParts(_)));
        if first_turn && !answered {
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "c1".into(),
                    name: "attach_file".into(),
                    arguments: json!({ "path": "pic.png" }).to_string(),
                },
                ResponseEvent::Done,
            ]
        } else {
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done]
        }
    })
    .with_vision();
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<_>)
        .toolset(Toolset::coding())
        .project_root(dir.path())
        .approvals(ApprovalPolicy::Auto)
        .build()
        .expect("an engine builds");
    let mut run = engine
        .load_harness_from_str(DOC)
        .expect("loads")
        .start(json!({}))
        .expect("starts")
        .in_workspace(dir.path())
        .expect("a directory");
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("ends")
        .expect("an outcome");
    assert!(matches!(&outcome.result, Some(Ok(_))), "{outcome:?}");
    (run, seen)
}

fn image_paths(body: &atif::MessageBody) -> Vec<String> {
    body.as_segments()
        .into_iter()
        .flatten()
        .filter_map(|s| match s {
            atif::ContentSegment::Image { source } => Some(source.path.clone()),
            atif::ContentSegment::Text { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn the_attachment_turn_and_a_returned_image_keep_their_images() {
    let (run, seen) = executed().await;
    let conversations = run.conversations().expect("an executed run has them");
    let (_, look) = conversations.iter().find(|(n, _)| n == "look").unwrap();
    let images = run.trajectory_images().expect("an executed run has them");

    // The user turn holds the prompt and the picture.
    let user = look
        .steps
        .iter()
        .find(|s| s.source == atif::StepOrigin::User)
        .expect("the user turn that carries the attachment is a step");
    let segments = user.message.as_segments().expect("a multimodal message");
    assert!(
        matches!(&segments[0], atif::ContentSegment::Text { text } if text.contains("Describe the picture")),
        "{segments:?}"
    );
    let attached = image_paths(&user.message);
    assert_eq!(attached.len(), 1, "{segments:?}");

    // The model was given the attachment and the tool's image, and the files
    // hold exactly those bytes.
    let seen = seen.lock().unwrap().clone();
    let stored = images
        .iter()
        .find(|i| i.path == attached[0])
        .expect("the bytes are kept under the path the step names");
    assert!(seen.contains(&stored.bytes), "not what the model was given");

    // The tool's image stays in the observation.
    let returned: Vec<String> = look
        .steps
        .iter()
        .flat_map(|s| s.observation.iter().flat_map(|o| &o.results))
        .filter_map(|r| r.content.as_ref())
        .flat_map(image_paths)
        .collect();
    assert_eq!(returned.len(), 1, "{look:#?}");
    let stored = images.iter().find(|i| i.path == returned[0]).unwrap();
    assert!(seen.contains(&stored.bytes), "not what the model was given");

    // Files are named by their digest, and written beside the trajectories.
    let out = tempfile::tempdir().unwrap();
    sven_sdk::write_trajectory_images(out.path(), &images).expect("written");
    assert_eq!(
        std::fs::read(out.path().join(&attached[0])).unwrap(),
        stored.bytes
    );
}

#[tokio::test]
async fn a_conversation_records_the_tools_its_requests_offered() {
    let (run, _) = executed().await;
    let conversations = run.conversations().expect("an executed run has them");
    let offered = |name: &str| -> Vec<String> {
        let (_, t) = conversations.iter().find(|(n, _)| n == name).unwrap();
        let mut names: Vec<String> = t
            .agent
            .tool_definitions
            .iter()
            .flatten()
            .map(|d| d["function"]["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    };
    assert_eq!(offered("look"), ["attach_file", "read_file", "write_file"]);
    assert_eq!(offered("plain"), ["read_file", "write_file"]);
}
