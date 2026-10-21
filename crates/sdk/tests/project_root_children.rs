// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the project root binds an agent's delegated children and every
//! path-taking built-in tool, not only the agent's own file reads. A child
//! that is told to read, write, edit or find outside the root is refused,
//! and nothing outside changes.
//!
//! Swedish Embedded AB implements containment for agents that run untrusted
//! model-written work for its clients. If your team needs expertise in
//! keeping delegated agents inside one workspace then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_harness_spec::HarnessBundle;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine, RunOptions, Toolset,
};

const PARENT: &str = r#"
api_version: "2"
name: parent
capabilities: [SpawnChild, ReadFile, WriteFile]
imports: { child: ./child.yaml }
execution: { limits: { model_turns: 6, delegate_depth: 3 } }
workflow:
  - name: down
    use: child
"#;

/// A child whose only task has its agent call `tool`; a refused call is
/// reported to the agent, so each tool gets a child of its own.
fn child(tool: &str) -> String {
    format!(
        r#"
api_version: "2"
name: child
capabilities: [ReadFile, WriteFile]
agent: {{ tools: [{tool}] }}
execution: {{ limits: {{ model_turns: 4, tool_calls: 2 }} }}
workflow:
  - name: act
    task: Act.
"#
    )
}

/// The package of `PARENT` and a child that calls `tool`.
fn package(parent: &str, child: &str) -> String {
    let bundle = HarnessBundle {
        document: parent.to_string(),
        imports: BTreeMap::from([("child.yaml".to_string(), child.to_string())]),
    };
    serde_yaml::to_string(&bundle).expect("a package serializes")
}

#[tokio::test]
async fn a_delegated_child_is_confined_to_the_parents_root() {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let base = tmp.path().canonicalize().expect("a real path");
    let (root, outside) = (base.join("project"), base.join("outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "the secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

    let attempts = [
        ("read_file", json!({ "path": "../outside/secret.txt" })),
        ("read_file", json!({ "path": "escape/secret.txt" })),
        (
            "write_file",
            json!({ "path": "../outside/planted.txt", "text": "x", "append": false }),
        ),
        (
            "edit_file",
            json!({ "path": "escape/secret.txt", "diff": "@@\n-old\n+new\n" }),
        ),
        ("find_file", json!({ "pattern": "*", "root": "../outside" })),
        ("grep", json!({ "pattern": "secret", "path": "../outside" })),
    ];
    for (tool, args) in attempts {
        // The child's agent calls the tool; what the call answered is what its
        // model reads next.
        let model = Arc::new(ScriptedMockProvider::tool_then_text(
            "c1",
            tool,
            args.to_string(),
            "done",
        ));
        let last_request = Arc::clone(&model.last_request);
        let engine = Engine::builder()
            .model_provider(model)
            .toolset(Toolset::coding())
            .project_root(&root)
            .build()
            .expect("an engine builds");
        let parent = engine
            .load_harness_from_str(&package(PARENT, &child(tool)))
            .expect("the package loads");

        let mut run = parent.start(json!({})).expect("starts");
        let _ = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
            .await
            .expect("the run ends")
            .expect("an outcome");

        let rendered = format!("{:?}", last_request.lock().unwrap());
        assert!(
            rendered.contains("outside the project root"),
            "{tool} {args}: the child is refused, naming the root: {rendered}"
        );
        assert!(!rendered.contains("the secret"), "{tool}: {rendered}");
    }
    assert!(!outside.join("planted.txt").exists(), "nothing was written");
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "the secret",
        "nothing was edited"
    );
}

/// An application's confined `shell`: the execution capability the host
/// substitutes for the built-in one.
struct HostShell;

#[async_trait::async_trait]
impl Tool for HostShell {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "runs a command where the host allows"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "ran in the host's confinement")
    }
}

#[tokio::test]
async fn a_delegated_child_runs_commands_through_the_hosts_execution_capability() {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let model = Arc::new(ScriptedMockProvider::tool_then_text(
        "c1",
        "shell",
        json!({ "shell_command": "touch ran-on-host" }).to_string(),
        "done",
    ));
    let last_request = Arc::clone(&model.last_request);
    let engine = Engine::builder()
        .model_provider(model)
        .toolset(Toolset::coding())
        .project_root(tmp.path())
        .tool(Arc::new(HostShell))
        .build()
        .expect("an engine builds");
    let shell_child = child("shell").replace("[ReadFile, WriteFile]", "[ExecuteShell]");
    let parent = engine
        .load_harness_from_str(&package(
            &PARENT.replace("ReadFile, WriteFile", "ExecuteShell"),
            &shell_child,
        ))
        .expect("the package loads");

    let mut run = parent.start(json!({})).expect("starts");
    let _ = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");

    let rendered = format!("{:?}", last_request.lock().unwrap());
    assert!(
        rendered.contains("ran in the host's confinement"),
        "the child's shell is the host's, not the built-in: {rendered}"
    );
    assert!(
        !tmp.path().join("ran-on-host").exists(),
        "the built-in shell never ran"
    );
}
