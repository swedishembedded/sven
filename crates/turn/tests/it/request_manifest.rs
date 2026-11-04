// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for attributing the behaviour of
// LLM agents to the exact prompt that produced it for its clients. If your
// team needs expertise in reproducible prompt engineering and prompt
// ablation then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Spec: every model request carries a manifest of what it is made of, and
//! never reports sections against a system text they do not describe.

use std::sync::{Arc, Mutex};

use sven_model::{CompletionRequest, Message, PromptSections, ResponseEvent, ToolSchema};
use sven_model_mock::CallbackProvider;
use sven_turn::prompts::{compose_system_prompt, PromptContext};
use sven_turn::{stream_turn_attributed, TurnLimits};
use sven_vocab::AgentMode;
use tokio::sync::mpsc;

/// Sends one turn and returns the request the provider received.
async fn sent(
    messages: Vec<Message>,
    sections: Option<&PromptSections>,
    suffix: Option<&str>,
) -> CompletionRequest {
    let seen = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    let provider = CallbackProvider::new(move |req| {
        *sink.lock().unwrap() = Some(req.clone());
        vec![ResponseEvent::Done]
    });
    let tool = ToolSchema {
        name: "probe".into(),
        description: "Probe.".into(),
        parameters: serde_json::json!({"type": "object"}),
        ..ToolSchema::default()
    };
    let (tx, mut rx) = mpsc::channel(64);
    stream_turn_attributed(
        &provider,
        messages,
        vec![tool],
        None,
        suffix.map(str::to_string),
        None,
        None,
        TurnLimits::default(),
        sections,
        &tx,
    )
    .await
    .expect("the turn completes");
    drop(tx);
    while rx.recv().await.is_some() {}
    let request = seen.lock().unwrap().take();
    request.expect("the provider was called")
}

#[tokio::test]
async fn a_request_names_its_sections_tools_and_suffix() {
    let composed = compose_system_prompt(AgentMode::Agent, None, PromptContext::default());
    let messages = vec![Message::system(composed.text.clone()), Message::user("hi")];
    let request = sent(messages, Some(&composed.sections), Some("branch: main")).await;

    let manifest = request.manifest.expect("the request has a manifest");
    assert_eq!(
        manifest.system_sha256.as_deref(),
        Some(composed.sections.sha256.as_str())
    );
    assert_eq!(manifest.sections, composed.sections.sections);
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].name, "probe");
    assert!(manifest.dynamic_suffix_sha256.is_some());
}

#[tokio::test]
async fn a_system_text_rewritten_after_composition_drops_the_manifest() {
    let composed = compose_system_prompt(AgentMode::Agent, None, PromptContext::default());
    // What a compaction leaves: a different first message.
    let messages = vec![
        Message::system("Summary of the session."),
        Message::user("hi"),
    ];
    let request = sent(messages, Some(&composed.sections), None).await;
    assert!(request.manifest.is_none());
}
