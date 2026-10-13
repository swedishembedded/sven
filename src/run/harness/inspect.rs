// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness inspect` and `schema`: what a harness is, and its contract.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::Path;

use sven_harness_testkit::{inspect as inspect_harness, io_schema, TestkitError};

use super::{kit, print_json, Failure, Outcome};

/// Prints what the harness at `path` is. An invalid harness is exit 1; a
/// path that cannot be read is exit 2.
pub(super) fn inspect(path: &Path, json: bool) -> Outcome {
    match inspect_harness(path) {
        Ok(report) => {
            if json {
                print_json(&report)?;
            } else {
                print_report(&report);
            }
            Ok(0)
        }
        Err(error @ (TestkitError::Harness(_) | TestkitError::Manifest(_))) => {
            if json {
                print_json(&serde_json::json!({"valid": false, "errors": [error.to_string()]}))?;
            } else {
                println!("INVALID {}: {error}", path.display());
            }
            Ok(1)
        }
        Err(other) => Err(kit(other)),
    }
}

/// Prints the document JSON Schema, or the input/output contract of the
/// harness at `path`.
pub(super) fn schema(path: Option<&Path>) -> Outcome {
    match path {
        None => println!("{}", sven_harness_spec::DOCUMENT_SCHEMA.trim_end()),
        Some(path) => {
            let contract = io_schema(path).map_err(|e| match e {
                TestkitError::Harness(_) => Failure {
                    code: 1,
                    message: e.to_string(),
                },
                other => kit(other),
            })?;
            print_json(&contract)?;
        }
    }
    Ok(0)
}

fn print_report(report: &sven_harness_testkit::InspectReport) {
    println!("{} ({})", report.name, report.path.display());
    println!("  plan digest: {}", report.plan_digest);
    if let Some(digest) = &report.bundle_digest {
        println!("  bundle digest: {digest}");
    }
    println!("  entry: {}", report.entry);
    println!("  capabilities: {}", report.capabilities.join(", "));
    for (label, fields) in [("inputs", &report.inputs), ("outputs", &report.outputs)] {
        let words: Vec<String> = fields.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        println!("  {label}: {}", words.join(", "));
    }
    println!("  steps:");
    for step in &report.steps {
        println!(
            "    {} ({}) -> {}",
            step.id,
            step.primitive,
            step.successors.join(", ")
        );
    }
    for helper in &report.helpers {
        println!(
            "  helper {} ({}, {} bytes)",
            helper.name, helper.abi, helper.bytes
        );
    }
    for child in &report.children {
        println!("  child {} ({})", child.name, child.path.display());
    }
    println!("  cases: {}", report.cases.join(", "));
}
