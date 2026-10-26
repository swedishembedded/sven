// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the reasoning a model produced in a harness step is part of the
//! step's recorded trajectory (`reasoning_content`), so thinking-mode runs
//! make usable training data. It is never part of what later requests carry.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use sven_model::{MessageContent, ResponseEvent};
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, RunOptions};

const DOC: &str = r#"
api_version: "2"
name: thought
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    task: Think, then say.
  - name: more
    task: Again.
"#;

#[tokio::test]
async fn reasoning_is_recorded_in_the_trajectory_and_never_sent_back() {
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&requests);
    let provider = CallbackProvider::new(move |req| {
        seen.lock().unwrap().push(
            req.messages
                .iter()
                .filter_map(|m| match &m.content {
                    MessageContent::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        vec![
            ResponseEvent::ThinkingDelta("secret chain of thought".into()),
            ResponseEvent::TextDelta("the answer".into()),
            ResponseEvent::Done,
        ]
    });
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<_>)
        .build()
        .expect("an engine builds");
    let mut run = engine
        .load_harness_from_str(DOC)
        .expect("loads")
        .start(json!({}))
        .expect("starts");
    tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("ends")
        .expect("an outcome");

    let conversations = run.conversations().expect("an executed run has them");
    let reasoning: Vec<_> = conversations
        .iter()
        .flat_map(|(_, c)| &c.steps)
        .filter_map(|s| s.reasoning_content.as_deref())
        .collect();
    assert_eq!(
        reasoning, ["secret chain of thought"; 2],
        "{conversations:#?}"
    );
    for request in requests.lock().unwrap().iter() {
        assert!(!request.contains("secret chain"), "{request}");
    }
}
