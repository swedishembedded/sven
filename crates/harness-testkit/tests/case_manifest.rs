// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness directory and its case manifests are strict data. Unknown
//! keys and ambiguous model fixtures are refused at parse time, and every
//! file a manifest names stays inside the harness directory.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod common;

use common::{harness_dir, write_tree, ROUTING};
use sven_harness_testkit::{CaseManifest, HarnessDir, ModelFixture};

const FULL_CASE: &str = r#"
name: bug
input: { ticket: "The app crashes on start" }
model:
  sequence:
    - text: '{"label": "bug"}'
      usage: { input: 20, output: 6 }
    - tool_calls: [ { id: c1, name: read_notes, args: { path: notes.md } } ]
tools:
  read_notes:
    capability: ReadFile
    responses:
      - ok: "remember the milk"
      - error: "disk full"
      - block: true
      - ok: "patched"
        write: { path: fixed.txt, content: "ok" }
workspace: { notes.md: "remember the milk" }
bounds: { deadline_ms: 300, max_output_tokens: 100, cancel_after_ms: 50 }
replay: true
expect:
  status: suspended
  output: { queue: bugs, label: bug }
  output_includes: { queue: bugs }
  failure: { code: unroutable, message_contains: "valid route" }
  steps: { classify: 1, ask: 1 }
  max_steps: 6
  tools_invoked: [read_notes]
  tools_not_invoked: [write_notes]
  forbidden_capabilities: [ExecuteShell, WriteFile]
  max_output_tokens: 100
resume:
  model: { sequence: [] }
  tools: { read_report: { capability: ReadFile, responses: [ { ok: "report" } ] } }
  expect: { status: completed, steps: { call: 2 } }
"#;

#[test]
fn the_full_manifest_example_parses() {
    let case = CaseManifest::from_yaml(FULL_CASE).expect("the documented example parses");
    assert_eq!(case.name, "bug");
    assert!(matches!(&case.model, Some(ModelFixture::Sequence(items)) if items.len() == 2));
    assert_eq!(case.tools["read_notes"].responses.len(), 4);
    assert_eq!(case.workspace["notes.md"], "remember the milk");
    assert_eq!(case.bounds.cancel_after_ms, Some(50));
    assert!(case.replay);
    let resume = case.resume.expect("the resume phase parses");
    assert_eq!(resume.expect.steps["call"], 2);
}

#[test]
fn the_case_name_defaults_to_the_file_stem() {
    let (tmp, _) = harness_dir(&[
        ("harness.yaml", ROUTING),
        (
            "cases/first-contact.yaml",
            "input: {}\nexpect: { status: completed }\n",
        ),
    ]);
    let case = CaseManifest::from_path(&tmp.path().join("cases/first-contact.yaml"))
        .expect("a minimal case parses");
    assert_eq!(case.name, "first-contact");
    assert!(
        case.replay,
        "the replay check is on unless a case turns it off"
    );
}

#[test]
fn an_unknown_key_is_rejected_at_every_level() {
    for text in [
        "input: {}\nexpect: { status: completed }\ntypo: 1\n",
        "input: {}\nexpect: { status: completed, outptu: 1 }\n",
        "input: {}\ntools: { t: { capability: ReadFile, responses: [], extra: 1 } }\nexpect: { status: completed }\n",
        "input: {}\nmodel: { sequence: [ { text: x, bogus: 1 } ] }\nexpect: { status: completed }\n",
    ] {
        assert!(
            CaseManifest::from_yaml(text).is_err(),
            "an unknown key must not be ignored: {text}"
        );
    }
}

#[test]
fn a_model_block_names_exactly_one_kind() {
    let two = "input: {}\nmodel: { sequence: [], fail: boom }\nexpect: { status: completed }\n";
    let error = CaseManifest::from_yaml(two).expect_err("two kinds are ambiguous");
    assert!(error.to_string().contains("exactly one"), "{error}");

    let none = "input: {}\nmodel: {}\nexpect: { status: completed }\n";
    assert!(
        CaseManifest::from_yaml(none).is_err(),
        "an empty block names no model"
    );

    for kind in [
        "sequence: []",
        "rules: []",
        "file: fixtures/model.yaml",
        "fail: down",
        "live: { model: \"ollama/qwen3:8b\" }",
    ] {
        let text = format!("input: {{}}\nmodel: {{ {kind} }}\nexpect: {{ status: completed }}\n");
        CaseManifest::from_yaml(&text).unwrap_or_else(|e| panic!("`{kind}` is a model kind: {e}"));
    }
}

#[test]
fn a_tool_response_does_exactly_one_thing() {
    let both = "input: {}\ntools: { t: { capability: ReadFile, responses: [ { ok: a, error: b } ] } }\nexpect: { status: completed }\n";
    assert!(
        CaseManifest::from_yaml(both).is_err(),
        "ok and error together are ambiguous"
    );
    let empty = "input: {}\ntools: { t: { capability: ReadFile, responses: [ {} ] } }\nexpect: { status: completed }\n";
    assert!(
        CaseManifest::from_yaml(empty).is_err(),
        "an empty response does nothing"
    );
}

#[test]
fn a_resume_phase_needs_a_suspended_first_phase() {
    let text =
        "input: {}\nexpect: { status: completed }\nresume: { expect: { status: completed } }\n";
    let error = CaseManifest::from_yaml(text).expect_err("nothing suspended, nothing to resume");
    assert!(error.to_string().contains("suspended"), "{error}");
}

#[test]
fn a_harness_directory_is_found_by_its_layout() {
    let (tmp, dir) = harness_dir(&[
        ("harness.yaml", ROUTING),
        ("cases/b.yaml", "x: 1"),
        ("cases/a.yaml", "x: 1"),
        ("cases/notes.txt", "not a case"),
    ]);
    assert_eq!(dir.main, tmp.path().join("harness.yaml"));
    let names = |paths: &[std::path::PathBuf]| -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(names(&dir.cases), ["a.yaml", "b.yaml"]);
}

#[test]
fn a_file_path_is_a_main_document_alone() {
    let tmp = tempfile::tempdir().unwrap();
    write_tree(tmp.path(), &[("only.yaml", ROUTING)]);
    let dir = HarnessDir::open(&tmp.path().join("only.yaml")).expect("a file opens");
    assert_eq!(dir.main, tmp.path().join("only.yaml"));
    assert!(dir.cases.is_empty());
}

#[test]
fn a_directory_needs_exactly_one_main_document() {
    let both = tempfile::tempdir().unwrap();
    write_tree(
        both.path(),
        &[("harness.yaml", ROUTING), ("bundle.yaml", ROUTING)],
    );
    let error = HarnessDir::open(both.path()).expect_err("two mains are ambiguous");
    assert!(error.to_string().contains("exactly one"), "{error}");

    let neither = tempfile::tempdir().unwrap();
    assert!(HarnessDir::open(neither.path()).is_err());
}

#[test]
fn cases_can_come_from_another_directory() {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", ROUTING)]);
    let cases = tempfile::tempdir().unwrap();
    write_tree(cases.path(), &[("one.yaml", "x: 1"), ("two.yaml", "x: 1")]);
    let dir = dir
        .with_cases_from(cases.path())
        .expect("a cases dir attaches");
    assert_eq!(dir.cases.len(), 2);
}

#[test]
fn resolve_stays_inside_the_harness_directory() {
    let (tmp, dir) = harness_dir(&[
        ("harness.yaml", ROUTING),
        ("fixtures/model.yaml", "responses: []"),
    ]);
    let found = dir
        .resolve("fixtures/model.yaml")
        .expect("a nested file resolves");
    assert_eq!(
        found,
        tmp.path()
            .canonicalize()
            .unwrap()
            .join("fixtures/model.yaml")
    );

    let outside = tempfile::tempdir().unwrap();
    let absolute = outside.path().join("secret.yaml");
    std::fs::write(&absolute, "x").unwrap();
    for escape in [
        absolute.to_str().unwrap(),
        "../outside.yaml",
        "fixtures/../../x.yaml",
    ] {
        assert!(dir.resolve(escape).is_err(), "`{escape}` must not resolve");
    }
}

#[cfg(unix)]
#[test]
fn resolve_rejects_a_symlink_that_leaves_the_directory() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.yaml"), "x").unwrap();
    let (tmp, dir) = harness_dir(&[("harness.yaml", ROUTING)]);
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("link")).unwrap();
    let error = dir
        .resolve("link/secret.yaml")
        .expect_err("a symlink is not a way out of the jail");
    assert!(error.to_string().contains("outside"), "{error}");
}
