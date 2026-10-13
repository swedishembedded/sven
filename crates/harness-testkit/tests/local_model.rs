// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Opt-in spec: the local-model sample passes against a model the caller is
//! already serving. Ignored by default - `make test`, `make check` and
//! `make harness/test` never reach it. Run it with `make harness/test/local`
//! after setting `SVEN_HARNESS_LOCAL_MODEL=1` and
//! `SVEN_HARNESS_LOCAL_ENDPOINT`; nothing here pulls or starts a model.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows against local models then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::Path;

use sven_harness_testkit::{run_suite, HarnessDir, OptIn, RunnerOptions};

#[tokio::test]
#[ignore = "needs a model you already serve: see make harness/test/local"]
async fn the_local_qwen_sample_passes_against_a_served_model() {
    let opt_in = OptIn::from_env();
    assert!(
        opt_in.is_live_ready(),
        "this test talks to a real model and stays off by default: serve one \
         yourself, then set SVEN_HARNESS_LOCAL_MODEL=1 and \
         SVEN_HARNESS_LOCAL_ENDPOINT=<url> (or run `make harness/test/local`)"
    );

    let sample =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/harnesses/08-local-qwen");
    let dir = HarnessDir::open(&sample).expect("the sample directory opens");
    let options = RunnerOptions {
        opt_in,
        ..RunnerOptions::default()
    };
    let report = run_suite(&dir, &options).await;

    assert!(report.valid, "the sample is invalid: {:?}", report.error);
    assert_eq!(
        report.skipped, 0,
        "an opted-in run skips nothing: {report:#?}"
    );
    assert_eq!(report.failed, 0, "{report:#?}");
    assert!(report.passed > 0, "the sample ran its cases: {report:#?}");
}
