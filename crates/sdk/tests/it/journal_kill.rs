// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a process killed in the middle of a run leaves a journal that parses,
//! holds the steps that had settled, and says it is incomplete.
//!
//! The test runs a real second process (this test binary, in a child mode) that
//! journals an agent run and then hangs inside a model request; the first
//! process kills it with SIGKILL and reads what was left.
//!
//! Swedish Embedded AB implements crash-robust agent telemetry for its
//! clients. If your team needs expertise in keeping agent run records intact
//! when machines fail then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atif::persist::{read_journal, FlushPolicy, JournalLimits, CAPTURE_STATUS_KEY};
use serde_json::{json, Value};
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};
use sven_sdk::tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_sdk::Engine;

/// What the child process finds in its working directory, which is how the
/// parent hands it its plumbing: a re-executed test binary has no flags of its
/// own, and the environment is not for settings.
const CHILD_MARKER: &str = "child-mode";
const CHILD_JOURNAL: &str = "run.journal";
const CHILD_READY: &str = "ready";

struct Notes;

#[async_trait::async_trait]
impl Tool for Notes {
    fn name(&self) -> &str {
        "read_notes"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Reads the notes."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "blue")
    }
}

/// Calls the tool twice, then hangs in the third request, after saying so.
struct HangsOnThirdRequest {
    calls: AtomicUsize,
    ready: PathBuf,
}

#[async_trait::async_trait]
impl ModelProvider for HangsOnThirdRequest {
    fn name(&self) -> &str {
        "hangs"
    }
    fn model_name(&self) -> &str {
        "hangs"
    }
    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n >= 2 {
            std::fs::write(&self.ready, b"hanging")?;
            futures::future::pending::<()>().await;
        }
        let events = vec![
            Ok(ResponseEvent::ToolCall {
                index: 0,
                id: format!("c{n}"),
                name: "read_notes".into(),
                arguments: "{}".into(),
            }),
            Ok(ResponseEvent::Usage {
                input_tokens: 10,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            }),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// The child process: not a test of its own, so it is ignored unless the
/// parent test starts it with its plumbing set.
#[tokio::test]
#[ignore = "runs only as the child of the kill test"]
async fn journaled_run_that_hangs() {
    if !Path::new(CHILD_MARKER).exists() {
        return;
    }
    let engine = Engine::builder()
        .model_provider(Arc::new(HangsOnThirdRequest {
            calls: AtomicUsize::new(0),
            ready: CHILD_READY.into(),
        }) as Arc<_>)
        .tool(Arc::new(Notes))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    agent
        .journal_to(
            Path::new(CHILD_JOURNAL),
            FlushPolicy::EveryRecord,
            JournalLimits::default(),
        )
        .expect("the journal opens");
    let _ = agent.send("read the notes twice").await;
}

#[test]
fn a_killed_run_leaves_a_parseable_incomplete_journal_of_its_settled_steps() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join(CHILD_JOURNAL);
    let ready = dir.path().join(CHILD_READY);
    std::fs::write(dir.path().join(CHILD_MARKER), b"").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "journaled_run_that_hangs",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(dir.path())
        .spawn()
        .expect("the child starts");

    let started = Instant::now();
    while !ready.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the child never reached its hanging request"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    child.kill().expect("SIGKILL the child");
    child.wait().expect("reaped");

    let read = read_journal(&journal).expect("a killed run's journal parses");
    assert!(!read.complete, "nothing closed it");
    assert_eq!(
        read.trajectory
            .extra
            .as_ref()
            .and_then(|e| e.get(CAPTURE_STATUS_KEY)),
        Some(&json!("incomplete"))
    );
    assert!(
        !read.trajectory.steps.is_empty(),
        "the steps that settled before the kill are there"
    );
    assert!(
        atif::validate_trajectory(&read.trajectory).is_ok(),
        "and they are a valid document"
    );
    let text = serde_json::to_string(&read.trajectory.steps).unwrap();
    assert!(text.contains("read the notes twice"));
}
