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

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_model_mock::FailingMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine, RunOptions, Toolset,
};

const PARENT: &str = r#"
api_version: "1"
name: parent
entry: down
capabilities: [SpawnChild, ReadFile, WriteFile]
limits: { max_steps: 20, max_delegate_depth: 3 }
steps:
  down:
    delegate: { target: child, store_as: got, then: done }
  done:
    return: { value: "{{ steps.down.value }}" }
"#;

/// A child whose only step is `tool` with `args`; a refused call fails the
/// child, so each tool gets a child of its own.
fn child(tool: &str, args: &str) -> String {
    format!(
        r#"
api_version: "1"
name: child
entry: act
capabilities: [ReadFile, WriteFile]
limits: {{ max_steps: 10 }}
steps:
  act:
    tool: {{ name: {tool}, args: {args}, store_as: out, then: done }}
  done:
    return: {{ value: "{{{{ steps.act.value }}}}" }}
"#
    )
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
        ("read_file", r#"{ path: "../outside/secret.txt" }"#),
        ("read_file", r#"{ path: "escape/secret.txt" }"#),
        (
            "write_file",
            r#"{ path: "../outside/planted.txt", text: "x", append: false }"#,
        ),
        (
            "edit_file",
            r#"{ path: "escape/secret.txt", diff: "@@\n-the secret\n+changed\n" }"#,
        ),
        ("find_file", r#"{ pattern: "*", root: "../outside" }"#),
        ("grep", r#"{ pattern: "secret", path: "../outside" }"#),
    ];
    for (tool, args) in attempts {
        let engine = Engine::builder()
            .model_provider(Arc::new(FailingMockProvider::new("no model here")) as Arc<_>)
            .toolset(Toolset::coding())
            .project_root(&root)
            .build()
            .expect("an engine builds");
        engine
            .load_harness_from_str(&child(tool, args))
            .expect("child loads");
        let parent = engine.load_harness_from_str(PARENT).expect("parent loads");

        let mut run = parent.start(json!({})).expect("starts");
        let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
            .await
            .expect("the run ends")
            .expect("an outcome");

        let rendered = format!("{:?}", outcome.result);
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
    let engine = Engine::builder()
        .model_provider(Arc::new(FailingMockProvider::new("no model here")) as Arc<_>)
        .toolset(Toolset::coding())
        .project_root(tmp.path())
        .tool(Arc::new(HostShell))
        .build()
        .expect("an engine builds");
    let shell_child = child("shell", "{ shell_command: \"touch ran-on-host\" }")
        .replace("[ReadFile, WriteFile]", "[ExecuteShell]");
    engine
        .load_harness_from_str(&shell_child)
        .expect("child loads");
    let parent = engine
        .load_harness_from_str(&PARENT.replace("ReadFile, WriteFile", "ExecuteShell"))
        .expect("parent loads");

    let mut run = parent.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");

    let rendered = format!("{:?}", outcome.result);
    assert!(
        rendered.contains("ran in the host's confinement"),
        "the child's shell is the host's, not the built-in: {rendered}"
    );
    assert!(
        !tmp.path().join("ran-on-host").exists(),
        "the built-in shell never ran"
    );
}
