// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every harness sample under `samples/harnesses/` validates and passes
//! its own cases on mock models. The one case that names a real local model
//! (`ticket-routing/cases/live-local.yaml`) only ever skips here - it needs an
//! explicit endpoint this suite never gives.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};

use sven_harness_testkit::{run_suite, CaseStatus, HarnessDir, OptIn, RunnerOptions};

fn samples_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/harnesses")
}

/// The harness samples shipped, by directory name.
const SAMPLES: [&str; 14] = [
    "branching",
    "custom-helper",
    "embedded-hardening",
    "module-sweep",
    "parallel-research",
    "planner-worker",
    "repair-loop",
    "repo-repair",
    "reviewed-change",
    "rust-refactor",
    "scoped-agent",
    "suspend-resume",
    "ticket-routing",
    "verified-task",
];

/// The one case that names a live model, and so only ever skips.
const LIVE_CASE: (&str, &str) = ("ticket-routing", "live-local");

/// The sample directories on disk: every directory holding a harness
/// document, sorted.
fn sample_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(samples_root())
        .expect("samples/harnesses exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            ["harness.yaml", "document.yaml", "bundle.yaml"]
                .iter()
                .any(|doc| path.join(doc).is_file())
        })
        .collect();
    dirs.sort();
    dirs
}

#[tokio::test]
async fn every_default_sample_validates_and_passes() {
    let dirs = sample_dirs();
    let names: Vec<String> = dirs
        .iter()
        .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, SAMPLES, "the thirteen named harness samples");
    let options = RunnerOptions {
        opt_in: OptIn::none(),
        ..RunnerOptions::default()
    };

    for path in dirs {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let dir = HarnessDir::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            !dir.cases.is_empty(),
            "{name}: a sample ships cases that show what it does"
        );
        let report = run_suite(&dir, &options).await;
        assert!(report.valid, "{name}: invalid: {:?}", report.error);
        assert_eq!(report.failed, 0, "{name}: {:#?}", report.cases);
        assert!(report.passed > 0, "{name}: runs and passes its cases");
        for case in &report.cases {
            let is_live = (name.as_str(), case.case.as_str()) == LIVE_CASE;
            if is_live {
                assert!(
                    matches!(case.status, CaseStatus::Skipped { .. }),
                    "{name}/{}: the live case only skips: {:?}",
                    case.case,
                    case.status
                );
            } else {
                assert_eq!(case.status, CaseStatus::Passed, "{name}/{}", case.case);
            }
        }
    }
}
