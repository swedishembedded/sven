// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sample-harnesses-sdk-runner DIR [CASE]`: run a harness directory's cases
//! through the public SDK. See README.md.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in running agent workflows from your own
//! application then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::PathBuf;
use std::process::ExitCode;

use sample_harnesses_sdk_runner::{run_case, run_dir, CaseResult, Outcome};

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next().map(PathBuf::from) else {
        eprintln!("usage: sample-harnesses-sdk-runner HARNESS_DIR [CASE_FILE]");
        return ExitCode::from(2);
    };
    let results: anyhow::Result<Vec<CaseResult>> = match args.next() {
        Some(case) => run_case(&dir, &PathBuf::from(case)).await.map(|r| vec![r]),
        None => run_dir(&dir).await,
    };
    let results = match results {
        Ok(results) => results,
        Err(error) => {
            eprintln!("sample-harnesses-sdk-runner: {error:#}");
            return ExitCode::from(2);
        }
    };

    let mut failed = 0usize;
    let mut skipped = 0usize;
    for result in &results {
        match &result.outcome {
            Outcome::Passed => println!("PASS {}", result.case),
            Outcome::Skipped(reason) => {
                skipped += 1;
                println!("SKIP {} ({reason})", result.case);
            }
            Outcome::Failed(reasons) => {
                failed += 1;
                println!("FAIL {}", result.case);
                for reason in reasons {
                    println!("     {reason}");
                }
            }
        }
    }
    let passed = results.len() - failed - skipped;
    if skipped == 0 {
        println!("{passed} passed, {failed} failed");
    } else {
        println!("{passed} passed, {failed} failed, {skipped} skipped");
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
