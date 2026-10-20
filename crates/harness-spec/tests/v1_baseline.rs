// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: version 1 harness documents compile to exactly the plans they
//! compiled to before the v2 language existed.
//!
//! A plan's digest is what a run pins and what a checkpoint refuses to
//! resume against, so a drift here would silently orphan every stored
//! checkpoint and recording. The golden table
//! (`tests/golden/v1_plan_digests.txt`) was captured from the unchanged
//! compiler over every shipped v1 document; a change to the v1 compile path
//! that moves a digest fails this test and must be justified, not blessed.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sven_harness_spec::{compile_str, HarnessBundle, PLAN_FORMAT_VERSION};

fn samples_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/harnesses")
}

/// Every v1 document the repository ships, as (sample-relative path, text).
fn v1_documents() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![samples_root()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("readable samples dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                // Fixture repositories and sdk runner sources are not documents.
                let skip = ["fixtures", "src", "cases", "tests"];
                if !skip
                    .iter()
                    .any(|s| path.file_name().is_some_and(|n| n == *s))
                {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "yaml") {
                let text = std::fs::read_to_string(&path).expect("readable document");
                if text.contains("api_version: \"1\"") || text.contains("api_version: '1'") {
                    let rel = path.strip_prefix(samples_root()).expect("under root");
                    found.push((rel.to_string_lossy().into_owned(), text));
                }
            }
        }
    }
    found.sort();
    found
}

/// The digest a document compiles to: a bundle through the bundle path (its
/// digest pins the helpers), a bare document through `compile_str`.
fn digest_of(text: &str) -> String {
    if text.starts_with("document:") || text.contains("\ndocument: ") {
        let bundle: HarnessBundle = serde_yaml::from_str(text).expect("bundle parses");
        return bundle.compile().expect("bundle compiles").digest;
    }
    compile_str(text).expect("v1 document compiles").digest
}

fn golden() -> BTreeMap<String, String> {
    let table = include_str!("golden/v1_plan_digests.txt");
    table
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (path, digest) = l.split_once(' ').expect("`path digest` per line");
            (path.to_string(), digest.trim().to_string())
        })
        .collect()
}

#[test]
fn v1_compiled_plan_unchanged() {
    let actual: BTreeMap<String, String> = v1_documents()
        .into_iter()
        .map(|(path, text)| (path, digest_of(&text)))
        .collect();
    let expected = golden();
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "the set of shipped v1 documents changed; add or remove its golden line deliberately"
    );
    for (path, digest) in &actual {
        assert_eq!(
            digest,
            &expected[path],
            "{path}: the v1 plan digest moved. Actual table:\n{}",
            actual
                .iter()
                .map(|(p, d)| format!("{p} {d}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
fn v1_plans_keep_the_v1_format_stamp() {
    assert_eq!(PLAN_FORMAT_VERSION, "harness-plan/1");
    let (_, text) = v1_documents()
        .into_iter()
        .find(|(p, _)| p.ends_with("branching/harness.yaml"))
        .expect("branching sample");
    let plan = compile_str(&text).expect("compiles");
    let json = serde_json::to_value(&plan).expect("serializes");
    let object = json.as_object().expect("a plan is an object");
    for v2_only in ["profile", "source_map", "format"] {
        assert!(
            !object.contains_key(v2_only),
            "a v1 plan serializes no `{v2_only}` field"
        );
    }
}
