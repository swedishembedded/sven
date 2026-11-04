// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a task can attach workspace files (images, audio) to its prompt, so
//! a vision model sees a drawing in the task's first message instead of only
//! when it chooses to open it. Repairs and follow-ups continue the same
//! conversation, which already holds the files.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in multimodal agent pipelines
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use sven_harness_spec::{compile_str, PlanPrimitive};

const DOC: &str = r#"
api_version: "2"
name: attach
inputs: { drawings: string }
execution: { limits: { model_turns: 20 } }
gates:
  ok: { command: ["true"] }
workflow:
  - name: read
    task: Describe the drawing.
    attach: ["{{ inputs.drawings }}", input/request-sketch.png]
    follow_ups: [{ task: Anything else? }]
    verify: [ok]
    repair: { attempts: 1, prompt: Look again. }
"#;

#[test]
fn the_files_go_with_the_tasks_first_turn_only() {
    let plan = compile_str(DOC).expect("compiles");
    let PlanPrimitive::Agent { attach, .. } = &plan.steps["read__work"].primitive else {
        panic!("an agent step");
    };
    let attached: Vec<String> = attach.iter().map(|t| String::from(t.clone())).collect();
    assert_eq!(attached.len(), 2, "{attached:?}");
    assert!(attached[0].contains("inputs.drawings"), "{attached:?}");
    for (id, step) in &plan.steps {
        if id != "read__work" {
            if let PlanPrimitive::Agent { attach, .. } = &step.primitive {
                assert!(attach.is_empty(), "`{id}` repeats the attachments");
            }
        }
    }
}

#[test]
fn an_attachment_stays_inside_the_workspace_and_needs_a_prompt() {
    for (bad, finding) in [
        ("/etc/passwd", "not a path inside the workspace"),
        ("input/../../secret.png", "not a path inside the workspace"),
    ] {
        let doc = DOC.replace("input/request-sketch.png", bad);
        let error = compile_str(&doc).unwrap_err().to_string();
        assert!(error.contains(finding), "{bad}: {error}");
    }
    let doc = DOC
        .replace(
            "    follow_ups: [{ task: Anything else? }]\n",
            "    model_only: true\n",
        )
        .replace("    repair: { attempts: 1, prompt: Look again. }\n", "");
    let error = compile_str(&doc).unwrap_err().to_string();
    assert!(
        error.contains("`attach` sends files with an agent task's prompt"),
        "{error}"
    );
}
