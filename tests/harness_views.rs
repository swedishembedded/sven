// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness compile | explain | status | inspect --graph | validate`
//! against workflows: the commands that let a
//! person read a harness, and a run's progress, in the author's terms
//! without running a model.
//!
//! The specs: a compiled plan is canonical and carries its digest; the graph
//! shows every step and groups a workflow's steps by task; `explain` speaks
//! of tasks, checks and repairs, and of any one generated step by its origin;
//! `status` says where a suspended run rests and whether the document still
//! matches it; the reading commands need no model or network.
//!
//! Swedish Embedded AB implements reviewable, declarative agent workflows for
//! its clients. If your team needs expertise in explaining what a long-running
//! agent job does before it runs then you can procure our services by
//! sending an email to info@swedishembedded.com.

mod harness_support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples/harnesses")
        .join(name)
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/harness")
        .join(name)
}

fn sven(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
    cmd.current_dir(cwd)
        .env("HOME", cwd)
        .env("XDG_CONFIG_HOME", cwd)
        .env_remove("SVEN_MODEL")
        .arg("harness");
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("the sven binary runs")
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("exited normally")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is pure JSON ({e}): {}\n{}",
            stdout(out),
            stderr(out)
        )
    })
}

fn verified_task() -> PathBuf {
    sample("verified-task")
}

#[test]
fn compile_prints_the_canonical_plan_and_it_is_the_same_every_time() {
    let tmp = tempfile::tempdir().unwrap();
    let first = run(sven(tmp.path()).arg("compile").arg(verified_task()));
    assert_eq!(code(&first), 0, "{}", stderr(&first));
    let second = run(sven(tmp.path()).arg("compile").arg(verified_task()));
    assert_eq!(first.stdout, second.stdout, "compiling is deterministic");

    let plan = stdout_json(&first);
    assert_eq!(plan["api_version"], "2");
    assert_eq!(plan["profile"], "coding");
    assert!(plan["source_map"]
        .as_object()
        .is_some_and(|m| !m.is_empty()));

    let inspected = run(sven(tmp.path())
        .arg("inspect")
        .arg(verified_task())
        .arg("--json"));
    assert_eq!(
        plan["digest"],
        stdout_json(&inspected)["plan_digest"],
        "the compiled plan carries the digest every other command reports"
    );
}

#[test]
fn compile_writes_yaml_to_a_file_that_means_the_same_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let json = run(sven(tmp.path()).arg("compile").arg(verified_task()));
    let file = tmp.path().join("plan.yaml");
    let out = run(sven(tmp.path())
        .arg("compile")
        .arg(verified_task())
        .args(["--format", "yaml", "-o"])
        .arg(&file));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "the plan went to the file");
    let yaml: Value = serde_yaml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(yaml, stdout_json(&json));
}

#[test]
fn compile_prints_a_plan_with_its_source_map_and_refuses_an_invalid_document() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path()).arg("compile").arg(fixture("decide")));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let plan = stdout_json(&out);
    assert_eq!(plan["api_version"], "2");
    assert!(plan["source_map"].is_object(), "{plan}");

    let out = run(sven(tmp.path()).arg("compile").arg(fixture("broken.yaml")));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(
        stdout(&out).is_empty(),
        "no plan is printed for a bad document"
    );
}

#[test]
fn validate_checks_version_two_documents_and_names_what_is_wrong() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path())
        .arg("validate")
        .arg(verified_task())
        .arg(sample("module-sweep"))
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout_json(&out)
        .as_array()
        .unwrap()
        .iter()
        .all(|report| report["valid"] == true));

    let bad = tmp.path().join("bad.yaml");
    std::fs::write(
        &bad,
        r#"api_version: "2"
name: bad
execution: { limits: { model_turns: 4 } }
workflow:
  - { name: only, task: "do it", verify: [no_such_gate] }
"#,
    )
    .unwrap();
    let out = run(sven(tmp.path()).arg("validate").arg(&bad).arg("--json"));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let report = stdout_json(&out);
    assert!(
        report[0]["errors"][0]
            .as_str()
            .is_some_and(|e| e.contains("no_such_gate")),
        "{report}"
    );
}

#[test]
fn the_dot_graph_has_every_step_and_groups_a_workflows_steps_by_task() {
    let tmp = tempfile::tempdir().unwrap();
    let steps = stdout_json(&run(sven(tmp.path())
        .arg("inspect")
        .arg(verified_task())
        .arg("--json")))["steps"]
        .clone();
    let out = run(sven(tmp.path())
        .arg("inspect")
        .arg(verified_task())
        .args(["--graph", "dot"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let dot = stdout(&out);
    assert!(dot.starts_with("digraph "), "{dot}");
    for step in steps.as_array().unwrap() {
        let id = step["id"].as_str().unwrap();
        assert!(
            dot.contains(&format!("\"{id}\"")),
            "step {id} is a node: {dot}"
        );
    }
    assert!(dot.contains("subgraph \"cluster_implement\""), "{dot}");
    assert!(
        dot.contains("pass") && dot.contains("fail"),
        "gate edges say which way: {dot}"
    );
    assert_eq!(dot.matches('{').count(), dot.matches('}').count(), "{dot}");
}

#[test]
fn the_mermaid_graph_is_a_flowchart_with_a_subgraph_per_task() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path())
        .arg("inspect")
        .arg(verified_task())
        .args(["--graph", "mermaid"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with("flowchart TD"), "{text}");
    assert!(text.contains("subgraph task_implement"), "{text}");
    assert_eq!(
        text.matches("subgraph ").count(),
        text.lines().filter(|l| l.trim() == "end").count(),
        "{text}"
    );
}

#[test]
fn a_graph_format_must_be_dot_or_mermaid_and_excludes_json() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path())
        .arg("inspect")
        .arg(verified_task())
        .args(["--graph", "svg"]));
    assert_eq!(code(&out), 2);
    let out = run(sven(tmp.path())
        .arg("inspect")
        .arg(verified_task())
        .args(["--graph", "dot", "--json"]));
    assert_eq!(code(&out), 2);
}

#[test]
fn explain_describes_a_workflow_in_the_authors_terms() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path()).arg("explain").arg(verified_task()));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    for needle in [
        "verified-task",
        "task implement",
        "checks: test",
        "repair: 1 attempt",
        "success",
        "coverage",
    ] {
        assert!(text.contains(needle), "missing `{needle}`:\n{text}");
    }
    assert!(
        !text.contains("implement__v0_test_a0"),
        "the narrative speaks of tasks, not generated ids:\n{text}"
    );
}

#[test]
fn explain_a_step_gives_its_origin_and_where_it_leads() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path())
        .arg("explain")
        .arg(verified_task())
        .args(["--step", "implement__v0_test_a0"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    for needle in [
        "implement__v0_test_a0",
        "gate",
        "task implement",
        "check test",
        "after the first work turn",
        "passes -> ",
        "fails -> implement__v0_test_a0_repair",
        "reached from implement__work",
    ] {
        assert!(text.contains(needle), "missing `{needle}`:\n{text}");
    }
}

#[test]
fn explain_names_the_workflow_and_refuses_an_unknown_step() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path()).arg("explain").arg(fixture("decide")));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("decide"), "{}", stdout(&out));

    let out = run(sven(tmp.path())
        .arg("explain")
        .arg(verified_task())
        .args(["--step", "nope"]));
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("implement__work"),
        "lists the steps: {}",
        stderr(&out)
    );
}

/// Runs the verified-task sample against a model that never answers, so that
/// the deadline suspends it in its first model turn, and returns the
/// checkpoint file it left.
fn suspended_checkpoint(setup: &harness_support::Setup) -> PathBuf {
    harness_support::suspended_checkpoint(setup, &verified_task(), r#"{"feature": "x"}"#)
}

#[test]
fn status_says_where_a_suspended_run_rests() {
    let setup = harness_support::setup();
    let tmp = setup.scratch.path();
    let checkpoint = suspended_checkpoint(&setup);
    let out = run(sven(tmp)
        .arg("status")
        .arg(verified_task())
        .arg("--checkpoint")
        .arg(&checkpoint)
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let status = stdout_json(&out);
    assert_eq!(status["harness"], "verified-task");
    assert_eq!(status["resumable"], true);
    assert_eq!(status["current"]["step"], "implement__work");
    assert_eq!(status["current"]["task"], "implement");
    assert_eq!(status["current"]["role"], "work");

    let text = run(sven(tmp)
        .arg("status")
        .arg(verified_task())
        .arg("--checkpoint")
        .arg(&checkpoint));
    assert_eq!(code(&text), 0, "{}", stderr(&text));
    assert!(
        stdout(&text).contains("suspended at implement__work"),
        "{}",
        stdout(&text)
    );
    assert!(
        stdout(&text).contains("task implement"),
        "{}",
        stdout(&text)
    );
}

#[test]
fn status_says_a_checkpoint_no_longer_fits_an_edited_document() {
    let setup = harness_support::setup();
    let tmp = setup.scratch.path();
    let checkpoint = suspended_checkpoint(&setup);
    let edited = tmp.join("edited");
    std::fs::create_dir_all(&edited).unwrap();
    let text = std::fs::read_to_string(verified_task().join("harness.yaml"))
        .unwrap()
        .replace("attempts: 1", "attempts: 2");
    std::fs::write(edited.join("harness.yaml"), text).unwrap();

    let out = run(sven(tmp)
        .arg("status")
        .arg(&edited)
        .arg("--checkpoint")
        .arg(&checkpoint)
        .arg("--json"));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let status = stdout_json(&out);
    assert_eq!(status["resumable"], false);
    assert!(
        status["reason"]
            .as_str()
            .is_some_and(|r| r.contains("digest")),
        "{status}"
    );
}

#[test]
fn status_refuses_a_file_that_is_not_a_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let junk = tmp.path().join("junk.json");
    std::fs::write(&junk, "{\"hello\": 1}").unwrap();
    let out = run(sven(tmp.path())
        .arg("status")
        .arg(verified_task())
        .arg("--checkpoint")
        .arg(&junk));
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).contains("checkpoint"), "{}", stderr(&out));
}

#[test]
fn explain_with_inputs_shows_each_prompt_as_the_model_will_get_it() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path()).arg("explain").arg(verified_task()).args([
        "--input",
        r#"{"feature": "add a {{ inputs.nope }} discount"}"#,
    ]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("Implement: add a {{ inputs.nope }} discount."),
        "the input is substituted, and braces inside it stay data:\n{text}"
    );
    assert!(text.contains("implement__work"), "{text}");
    assert!(
        text.contains("sh run_tests.sh --coverage"),
        "gate argv is shown too:\n{text}"
    );
}

#[test]
fn explain_leaves_a_hole_it_cannot_fill_visible_and_elides_long_prompts() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path())
        .arg("explain")
        .arg(verified_task())
        .args(["--input", "{}", "--elide", "12"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("more characters"),
        "long prompts are cut:\n{text}"
    );

    let out = run(sven(tmp.path()).arg("explain").arg(verified_task()).args([
        "--step",
        "implement__work",
        "--input",
        "{}",
    ]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("{{inputs.feature}}"),
        "an input that was not given stays a visible hole:\n{}",
        stdout(&out)
    );
}

#[test]
fn explain_refuses_inputs_that_are_not_a_json_object() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(sven(tmp.path())
        .arg("explain")
        .arg(verified_task())
        .args(["--input", "[1]"]));
    assert_eq!(code(&out), 2, "{}", stderr(&out));
}
