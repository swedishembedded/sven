// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every harness sample under `samples/harnesses/` validates and passes
//! its own cases on mock models. The one sample that names a real local model
//! only ever skips here - it needs an explicit opt-in this suite never gives.
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

/// The numbered sample directories, sorted.
fn sample_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(samples_root())
        .expect("samples/harnesses exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path.file_name().is_some_and(|n| {
                    n.to_string_lossy()
                        .starts_with(|c: char| c.is_ascii_digit())
                })
        })
        .collect();
    dirs.sort();
    dirs
}

#[tokio::test]
async fn every_default_sample_validates_and_passes() {
    let dirs = sample_dirs();
    assert_eq!(dirs.len(), 8, "eight numbered harness samples: {dirs:?}");
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
        if name.starts_with("08-") {
            assert_eq!(report.passed, 0, "{name} is opt-in only");
            assert!(report.skipped > 0, "{name} reports its cases as skipped");
            assert!(report
                .cases
                .iter()
                .all(|c| matches!(c.status, CaseStatus::Skipped { .. })));
        } else {
            assert_eq!(report.skipped, 0, "{name}: no default sample skips");
            assert!(report.passed > 0, "{name}: runs and passes its cases");
        }
    }
}
