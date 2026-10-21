// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every harness run owns a private directory. The host may name it, else
//! the document may, else the run gets a fresh one under the system temporary
//! directory (mode 0700). It is never inside the workspace and never holds it,
//! a checkpoint carries it so a resume works in the same place, gates find it
//! in `SVEN_SCRATCHPAD`, and the run's audit log is written there, so a run
//! with the coding tools leaves no `.sven` in the repository it worked on.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in keeping the files of an
//! automated run out of the repository it works on then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sven_model::ResponseEvent;
use sven_model_mock::{CallbackProvider, ScriptedMockProvider};
use sven_sdk::{ApprovalPolicy, Engine, HarnessCheckpoint, HarnessOutcome, HarnessRun};
use sven_sdk::{RunOptions, Toolset};

async fn execute(run: &mut HarnessRun) -> HarnessOutcome {
    tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome")
}

fn text_engine(reply: &'static str) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::always_text(reply)) as Arc<_>)
        .build()
        .expect("an engine builds")
}

/// A gate that records what it was told the scratchpad is, twice over.
const GATED: &str = r#"
api_version: "2"
name: gated
execution: { limits: { model_turns: 4 } }
gates:
  note:
    run: |
      printf %s "$SVEN_SCRATCHPAD" > "$SVEN_SCRATCHPAD/from_env"
      printf %s "$PAD" > "$SVEN_SCRATCHPAD/from_template"
    env: { PAD: "{{ run.scratchpad }}" }
workflow:
  - name: work
    task: Work.
    verify: [note]
"#;

#[tokio::test]
async fn a_run_gets_a_private_directory_outside_the_workspace_that_gates_find() {
    let workspace = tempfile::tempdir().unwrap();
    let harness = text_engine("done")
        .load_harness_from_str(GATED)
        .expect("loads");
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    let id = run.run_id().to_string();
    assert!(run.scratchpad().is_none(), "not decided before it runs");
    let outcome = execute(&mut run).await;
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );

    let pad = run.scratchpad().expect("decided").to_path_buf();
    assert_eq!(
        pad,
        std::env::temp_dir()
            .join(format!("sven-harness-{id}"))
            .canonicalize()
            .unwrap_or(pad.clone())
    );
    assert!(!pad.starts_with(workspace.path()));
    let read = |name: &str| std::fs::read_to_string(pad.join(name)).expect(name);
    assert_eq!(read("from_env"), pad.to_string_lossy());
    assert_eq!(read("from_template"), pad.to_string_lossy());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&pad).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "only its owner may enter");
    }
    assert_eq!(
        std::fs::read_dir(workspace.path()).unwrap().count(),
        0,
        "the workspace was not written to"
    );
    let _ = std::fs::remove_dir_all(pad);
}

#[tokio::test]
async fn the_host_names_the_directory_and_keeps_it() {
    let workspace = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let chosen = base.path().join("deep").join("pad");
    let harness = text_engine("done")
        .load_harness_from_str(GATED)
        .expect("loads");
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap()
        .with_scratchpad(&chosen);
    execute(&mut run).await;
    assert_eq!(
        run.scratchpad().map(Path::to_path_buf),
        chosen.canonicalize().ok()
    );
    assert!(chosen.join("from_env").is_file());
}

const DOCUMENTED: &str = r#"
api_version: "2"
name: documented
inputs: { base: string }
execution:
  limits: { model_turns: 4 }
  scratchpad: "{{ inputs.base }}/named-by-document"
workflow:
  - name: work
    task: "Work in {{ run.scratchpad }}."
"#;

#[tokio::test]
async fn the_document_names_it_unless_the_host_does() {
    let workspace = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let harness = text_engine("done")
        .load_harness_from_str(DOCUMENTED)
        .expect("loads");
    let mut run = harness
        .start(json!({ "base": base.path() }))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    execute(&mut run).await;
    assert_eq!(
        run.scratchpad().map(Path::to_path_buf),
        base.path().join("named-by-document").canonicalize().ok()
    );

    let other = base.path().join("host-wins");
    let mut run = harness
        .start(json!({ "base": base.path() }))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap()
        .with_scratchpad(&other);
    execute(&mut run).await;
    assert_eq!(
        run.scratchpad().map(Path::to_path_buf),
        other.canonicalize().ok()
    );
}

#[tokio::test]
async fn a_directory_in_the_workspace_or_holding_it_is_refused_before_anything_runs() {
    let outer = tempfile::tempdir().unwrap();
    let workspace = outer.path().join("repo");
    std::fs::create_dir(&workspace).unwrap();
    let harness = text_engine("done")
        .load_harness_from_str(GATED)
        .expect("loads");
    for (what, dir, expect) in [
        ("inside", workspace.join("pad"), "inside the workspace"),
        (
            "holding",
            outer.path().to_path_buf(),
            "contains the workspace",
        ),
        ("the workspace", workspace.clone(), "inside the workspace"),
        ("relative", PathBuf::from("pad"), "absolute"),
        ("parent steps", workspace.join("..").join("x"), ".."),
    ] {
        let mut run = harness
            .start(json!({}))
            .unwrap()
            .in_workspace(&workspace)
            .unwrap()
            .with_scratchpad(&dir);
        let refused = run
            .execute(RunOptions::new())
            .await
            .expect_err("the directory is refused");
        assert!(refused.to_string().contains(expect), "{what}: {refused}");
    }
    assert_eq!(
        std::fs::read_dir(&workspace).unwrap().count(),
        0,
        "refusing it created nothing in the workspace"
    );
}

#[tokio::test]
async fn a_link_into_the_workspace_does_not_hide_a_directory_in_it() {
    let outer = tempfile::tempdir().unwrap();
    let workspace = outer.path().join("repo");
    std::fs::create_dir(&workspace).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&workspace, outer.path().join("link")).unwrap();
        let harness = text_engine("done")
            .load_harness_from_str(GATED)
            .expect("loads");
        let mut run = harness
            .start(json!({}))
            .unwrap()
            .in_workspace(&workspace)
            .unwrap()
            .with_scratchpad(outer.path().join("link").join("pad"));
        let refused = run.execute(RunOptions::new()).await.expect_err("refused");
        assert!(
            refused.to_string().contains("inside the workspace"),
            "{refused}"
        );
    }
}

const SWEEP: &str = r#"
api_version: "2"
name: sweep
inputs: { modules: json }
execution: { limits: { model_turns: 20 } }
workflow:
  - name: each
    for_each: { source: { input: modules }, id: name, max_items: 10 }
    checkpoint: each_item
    task: "Work on {{ item.name }} in {{ run.scratchpad }}."
success:
  work_items: { all_completed: true }
"#;

#[tokio::test]
async fn a_resumed_run_works_in_the_directory_its_checkpoint_recorded_unless_told_otherwise() {
    let workspace = tempfile::tempdir().unwrap();
    let prompts: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&prompts);
    let engine = Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |req| {
            let last = req
                .messages
                .last()
                .and_then(|m| m.as_text())
                .unwrap_or_default();
            seen.lock().unwrap().push(last.to_string());
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done]
        })) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(SWEEP).expect("loads");
    let checkpoints: Arc<Mutex<Vec<HarnessCheckpoint>>> = Arc::default();
    let sink = Arc::clone(&checkpoints);
    let base = tempfile::tempdir().unwrap();
    let first_pad = base.path().join("first");
    let mut run = harness
        .start(json!({"modules": [{"name": "a"}, {"name": "b"}, {"name": "c"}]}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap()
        .with_scratchpad(&first_pad)
        .on_item_checkpoint(move |c| sink.lock().unwrap().push(c));
    execute(&mut run).await;
    let checkpoint = checkpoints.lock().unwrap()[0].clone();
    let recorded = first_pad.canonicalize().unwrap();

    prompts.lock().unwrap().clear();
    let mut resumed = harness
        .resume(checkpoint.clone())
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    execute(&mut resumed).await;
    assert_eq!(resumed.scratchpad(), Some(recorded.as_path()));
    let said = prompts.lock().unwrap().join("\n");
    assert!(
        said.contains(&recorded.to_string_lossy().into_owned()),
        "{said}"
    );

    prompts.lock().unwrap().clear();
    let moved = base.path().join("second");
    let mut resumed = harness
        .resume(checkpoint)
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap()
        .with_scratchpad(&moved);
    execute(&mut resumed).await;
    let said = prompts.lock().unwrap().join("\n");
    let moved = moved.canonicalize().unwrap();
    assert!(
        said.contains(&moved.to_string_lossy().into_owned()),
        "{said}"
    );
    assert!(
        !said.contains(&recorded.to_string_lossy().into_owned()),
        "{said}"
    );
}

const CODING: &str = r#"
api_version: "2"
name: coding
agent: { profile: coding }
execution: { limits: { model_turns: 6 } }
workflow:
  - name: work
    task: Write out.txt.
"#;

#[tokio::test]
async fn a_run_with_the_coding_tools_audits_into_its_scratchpad_and_leaves_no_dot_sven_behind() {
    let workspace = tempfile::tempdir().unwrap();
    let provider = ScriptedMockProvider::tool_then_text(
        "call-1",
        "write_file",
        r#"{"path":"out.txt","text":"hello","append":false}"#,
        "wrote it",
    );
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<_>)
        .toolset(Toolset::coding())
        .project_root(workspace.path())
        .approvals(ApprovalPolicy::Auto)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(CODING).expect("loads");
    let mut run = harness.start(json!({})).unwrap();
    let outcome = execute(&mut run).await;
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("out.txt")).unwrap(),
        "hello",
        "the tool call really ran"
    );
    assert!(
        !workspace.path().join(".sven").exists(),
        "the audit log is not written into the repository"
    );
    let pad = run.scratchpad().expect("decided").to_path_buf();
    let audit = std::fs::read_to_string(pad.join("audit.jsonl")).expect("the audit log is there");
    assert!(audit.contains("write_file"), "{audit}");
    let _: Value = serde_json::from_str(audit.lines().next().unwrap()).expect("JSON lines");
    let _ = std::fs::remove_dir_all(pad);
}
