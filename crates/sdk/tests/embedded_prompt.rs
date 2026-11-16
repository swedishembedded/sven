// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for embedding agents safely in
// applications for its clients. If your team needs expertise in isolating an
// embedded agent from its host user's files then you can procure our services
// by sending an email to info@swedishembedded.com.
//! Spec: an embedded engine sends the system prompt its configuration names,
//! and reads no skills or sub-agents from the host user's home directory
//! unless the application opts in.

use std::sync::{Arc, Mutex};

use sven_model::{ResponseEvent, Role};
use sven_model_mock::CallbackProvider;
use sven_sdk::config::Config;
use sven_sdk::Engine;

/// The system message the model is sent for one turn of an engine built by
/// `build` over a home directory that holds a skill named `leakme`.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn system_sent(
    build: impl FnOnce(&mut Config, sven_sdk::EngineBuilder) -> sven_sdk::EngineBuilder,
) -> String {
    // The home directory is process-wide state: one run at a time.
    let _serial = SERIAL.lock().await;
    let home = tempfile::tempdir().unwrap();
    let skill = home.path().join(".cursor/skills/leakme");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: leakme\ndescription: A skill from the host user's home.\n---\nbody\n",
    )
    .unwrap();
    std::env::set_var("HOME", home.path());
    std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
    let workspace = tempfile::tempdir().unwrap();
    let sent = Arc::new(Mutex::new(String::new()));
    let seen = Arc::clone(&sent);
    let provider = CallbackProvider::new(move |req| {
        let system = req.messages.iter().find(|m| m.role == Role::System);
        *seen.lock().unwrap() = system.and_then(|m| m.as_text()).unwrap_or_default().into();
        vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done]
    });
    let mut config = Config::default();
    let builder = Engine::builder().model_provider(Arc::new(provider) as Arc<_>);
    let builder = build(&mut config, builder)
        .config(config)
        .project_root(workspace.path());
    builder
        .build()
        .unwrap()
        .agent("agent")
        .send("hi")
        .await
        .unwrap();
    let text = sent.lock().unwrap().clone();
    text
}

#[tokio::test]
async fn the_configured_system_prompt_is_what_is_sent() {
    let sent = system_sent(|config, b| {
        config.agent.system_prompt = Some("Exact embedded prompt.".into());
        b
    })
    .await;
    assert_eq!(sent, "Exact embedded prompt.");
}

#[tokio::test]
async fn skills_in_the_host_users_home_are_not_offered_unless_the_application_opts_in() {
    let default = system_sent(|_, b| b).await;
    assert!(!default.contains("leakme"), "{default}");
    let opted_in = system_sent(|_, b| b.host_instructions(true)).await;
    assert!(opted_in.contains("leakme"), "{opted_in}");
}
