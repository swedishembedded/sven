// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: which task graphs `plan_tasks` accepts, and what it tells the model
//! about the ones it refuses.

use serde_json::{json, Value};

use super::{owns_overlap, Limits, Plan};

fn limits() -> Limits {
    Limits {
        max_parallel: 4,
        max_fanout: 3,
        max_tasks: 5,
        max_graphs: 2,
    }
}

fn task(id: &str, owns: &[&str], deps: &[&str]) -> Value {
    json!({"id": id, "title": id, "prompt": format!("do {id}"), "owns": owns,
           "acceptance": "tests pass", "depends_on": deps})
}

fn refusal(tasks: Vec<Value>) -> String {
    let plan = Plan::parse(&json!({"tasks": tasks})).expect("well-formed");
    plan.validate(&limits()).unwrap_err().join("\n")
}

#[test]
fn independent_tasks_with_disjoint_files_are_accepted() {
    let plan = Plan::parse(&json!({
        "tasks": [task("a", &["src/a.py"], &[]), task("b", &["src/b.py"], &[]),
                  task("c", &["src/c.py"], &["a", "b"])],
        "verify_command": " pytest ",
    }))
    .unwrap();
    plan.validate(&limits()).unwrap();
    assert_eq!(plan.verify_command.as_deref(), Some("pytest"));
}

#[test]
fn a_cycle_names_the_tasks_on_it() {
    let said = refusal(vec![
        task("a", &["a"], &["b"]),
        task("b", &["b"], &["a"]),
        task("c", &["c"], &[]),
    ]);
    assert!(said.contains("cycle") && said.contains("a, b"), "{said}");
    assert!(!said.contains("a, b, c"), "{said}");
}

#[test]
fn an_unknown_dependency_and_a_repeated_id_are_named() {
    let said = refusal(vec![task("a", &["a"], &["ghost"]), task("a", &["b"], &[])]);
    assert!(said.contains("duplicate task id `a`"), "{said}");
    assert!(said.contains("unknown task `ghost`"), "{said}");
}

#[test]
fn an_empty_brief_or_missing_ownership_is_refused() {
    let mut empty = task("a", &["a"], &[]);
    empty["prompt"] = json!("  ");
    let said = refusal(vec![empty, task("b", &[], &[])]);
    assert!(said.contains("task `a`: `prompt` is empty"), "{said}");
    assert!(said.contains("task `b`: `owns` is empty"), "{said}");
}

#[test]
fn ownership_cannot_leave_the_project() {
    let said = refusal(vec![task("a", &["../x", "/etc"], &[])]);
    assert!(said.contains("`../x`") && said.contains("`/etc`"), "{said}");
}

#[test]
fn too_many_tasks_or_too_wide_a_layer_is_refused() {
    let many: Vec<Value> = (0..6)
        .map(|i| task(&format!("t{i}"), &[&format!("f{i}")], &[]))
        .collect();
    let said = refusal(many);
    assert!(
        said.contains("6 tasks submitted") && said.contains("at most 5"),
        "{said}"
    );
    assert!(said.contains("side by side"), "{said}");
}

#[test]
fn concurrent_tasks_may_not_share_files_but_ordered_ones_may() {
    let said = refusal(vec![
        task("a", &["src/pkg"], &[]),
        task("b", &["src/pkg/mod.py"], &[]),
    ]);
    assert!(
        said.contains("`a` and `b`") && said.contains("same time"),
        "{said}"
    );

    let ordered = Plan::parse(&json!({"tasks": [
        task("a", &["src/pkg"], &[]),
        task("m", &["m"], &["a"]),
        task("b", &["src/pkg/mod.py"], &["m"]),
    ]}))
    .unwrap();
    ordered.validate(&limits()).unwrap();
}

#[test]
fn malformed_shapes_are_reported_by_parse() {
    assert!(Plan::parse(&json!({})).is_err());
    let bad = Plan::parse(&json!({"tasks": [{"id": "a", "owns": "src"}]})).unwrap_err();
    assert!(bad[0].contains("`owns` is a list"), "{bad:?}");
}

#[test]
fn ownership_overlap_is_conservative_for_globs() {
    assert!(owns_overlap("src", "src/a.py"));
    assert!(owns_overlap("./src/", "src/a.py"));
    assert!(owns_overlap("src/*.py", "src/a.py"));
    assert!(owns_overlap("src/**", "src/pkg/a.py"));
    assert!(owns_overlap("src/**/*.py", "src/*.py"));
    assert!(!owns_overlap("src/a.py", "src/b.py"));
    assert!(!owns_overlap("src/*.py", "src/b.txt"));
    assert!(!owns_overlap("tests", "src"));
    assert!(!owns_overlap("src/a", "src/ab"));
}
