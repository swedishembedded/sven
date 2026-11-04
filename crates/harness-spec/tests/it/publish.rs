// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: `publish:` lowers to one publish step between its task and what
//! follows, derives `ReadFile`, and is refused where a task finishes more
//! than once or names a path outside the workspace; `retry: never` reaches
//! the plan; a `file` input takes a workspace-relative path.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde_json::json;
use sven_harness_spec::{compile_str, IOType, PlanPrimitive};

const DOC: &str = r#"
api_version: "2"
name: design
inputs: { drawing: file }
execution: { limits: { model_turns: 10 } }
workflow:
  - name: model
    retry: never
    task: "Model {{ inputs.drawing }}."
    publish: [part.step, "{{ inputs.drawing }}.notes"]
  - name: review
    task: "Review."
"#;

#[test]
fn a_publication_lowers_to_one_step_between_its_task_and_the_next() {
    let plan = compile_str(DOC).expect("compiles");
    let step = plan.step("model__publish").expect("a publish step");
    let PlanPrimitive::Publish { task, files, then } = &step.primitive else {
        panic!("{:?}", step.primitive);
    };
    assert_eq!(task, "model");
    assert_eq!(files.len(), 2);
    assert!(then.starts_with("review"), "{then}");
    assert_eq!(plan.source_map["model__publish"].role, "publish");
    assert!(
        plan.capabilities.iter().any(|c| c == "ReadFile"),
        "{:?}",
        plan.capabilities
    );
    assert!(plan.no_retry.contains("model"));
    assert!(!plan.no_retry.contains("review"));
}

#[test]
fn publish_is_refused_where_a_task_finishes_more_than_once_or_leaves_the_workspace() {
    let looped = DOC.replace(
        "    task: \"Model {{ inputs.drawing }}.\"",
        "    for_each: { source: { input: drawing }, id: name, max_items: 2 }\n    task: \"x\"",
    );
    assert!(
        compile_str(&looped).is_err(),
        "a loop publishes from a task after it"
    );
    for bad in ["/etc/passwd", "../x", ""] {
        let doc = DOC.replace("part.step", &format!("\"{bad}\""));
        let refused = compile_str(&doc).unwrap_err().to_string();
        assert!(refused.contains("publish"), "{bad:?}: {refused}");
    }
}

#[test]
fn a_file_input_is_a_workspace_relative_path() {
    assert!(IOType::File.matches(&json!("drawings/front.png")));
    for bad in [
        json!("/etc/passwd"),
        json!("../up"),
        json!("a/../../b"),
        json!(""),
        json!(3),
    ] {
        assert!(!IOType::File.matches(&bad), "{bad}");
    }
}
