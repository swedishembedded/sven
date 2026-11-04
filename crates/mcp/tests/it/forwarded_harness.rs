// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness can keep its plan in one process and run its tools in
//! others. Its agent's tools forward to a sandbox and its gates to a
//! verifier, each a `sven mcp serve --listen` that takes only its own token.
//! The gate passes on the verifier's own files, and the agent, whose tools
//! reach only the sandbox, cannot read the gate's script.
//!
//! Swedish Embedded AB implements solutions for running agent work safely
//! across isolated machines for its clients. If your team needs expertise in
//! sandboxed agent execution then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use sven_mcp_client::{ForwardedTool, McpManager, McpServerConfig, TokenStore};
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, RunOptions};
use sven_tool_api::PathScope;

const SECRET: &str = "SECRET-METHOD-7f3a";

const PLAN: &str = r#"
api_version: "2"
name: verified
execution: { limits: { model_turns: 6 } }
gates:
  check: { command: [sh, bin/check.sh] }
workflow:
  - name: work
    task: "Make the part."
    tools: [read_file]
    verify: [check]
"#;

/// A `sven mcp serve --listen` over `root` with `tools`, and its URL.
async fn serve(root: &std::path::Path, tools: &str, token: &str) -> String {
    let scope = PathScope::confined(root).unwrap();
    let registry = Arc::new(sven_mcp::build_mcp_registry(
        None,
        Some(tools),
        &sven_bootstrap::Config::default().tools,
        scope,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let token = token.to_string();
    tokio::spawn(async move { sven_mcp::serve_http(registry, listener, token).await });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_and_gates_run_elsewhere_and_the_agent_never_sees_the_method() {
    // The sandbox holds the work; the verifier holds the method and a copy
    // of the work to judge.
    let sandbox = tempfile::tempdir().unwrap();
    std::fs::write(sandbox.path().join("part.txt"), "bracket").unwrap();
    let verifier = tempfile::tempdir().unwrap();
    std::fs::create_dir(verifier.path().join("bin")).unwrap();
    std::fs::write(
        verifier.path().join("bin/check.sh"),
        format!("# {SECRET}\ntest -f part.txt\n"),
    )
    .unwrap();
    std::fs::write(verifier.path().join("part.txt"), "bracket").unwrap();

    let sandbox_token = "s".repeat(40);
    let verifier_token = "v".repeat(40);
    let sandbox_url = serve(sandbox.path(), "read_file", &sandbox_token).await;
    let verifier_url = serve(verifier.path(), "shell_gate", &verifier_token).await;

    // A wrong token reaches nothing.
    let refused = sven_http::client()
        .post(&sandbox_url)
        .header("Authorization", "Bearer wrong")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);

    let home = tempfile::tempdir().unwrap();
    let (events, _) = tokio::sync::mpsc::channel(64);
    let servers: HashMap<String, McpServerConfig> = [
        (
            "sandbox".to_string(),
            McpServerConfig::bearer_http(&sandbox_url, &sandbox_token),
        ),
        (
            "verifier".to_string(),
            McpServerConfig::bearer_http(&verifier_url, &verifier_token),
        ),
    ]
    .into_iter()
    .collect();
    let manager = McpManager::with_token_store(
        servers,
        events,
        false,
        TokenStore::new(home.path().join("t.json")),
    );
    manager.connect("sandbox").await.unwrap();
    manager.connect("verifier").await.unwrap();
    let mut tools =
        ForwardedTool::all(&manager, "sandbox", sven_hsm::capability_for_tool_name).await;
    tools
        .extend(ForwardedTool::all(&manager, "verifier", sven_hsm::capability_for_tool_name).await);
    let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(names, ["read_file", "shell_gate"]);

    // The agent tries to read the gate's script, then answers.
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = Arc::clone(&seen);
    let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut builder =
        Engine::builder().model_provider(Arc::new(CallbackProvider::new(move |request| {
            record
                .lock()
                .unwrap()
                .push(format!("{:?}", request.messages));
            match turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => vec![
                    ResponseEvent::ToolCall {
                        index: 0,
                        id: "r1".into(),
                        name: "read_file".into(),
                        arguments: json!({ "path": "bin/check.sh" }).to_string(),
                    },
                    ResponseEvent::Done,
                ],
                _ => vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done],
            }
        })) as Arc<_>);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let engine = builder.build().expect("an engine builds");
    let harness = engine.load_harness_from_str(PLAN).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(60), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "the gate passed in the verifier: {:?}",
        outcome.result
    );

    let seen = seen.lock().unwrap().join("\n");
    assert!(seen.contains("check.sh"), "the agent asked for the script");
    assert!(
        !seen.contains(SECRET),
        "the agent never saw the gate's script"
    );
}
