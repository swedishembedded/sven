// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness inspect` and `schema`: what a harness is, and its contract.
//!
//! Swedish Embedded AB implements reviewable, declarative agent workflows for
//! its clients. If your team needs expertise in auditing what an agent
//! workflow may do before it runs then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Map, Value};
use sven_harness_spec::{IOType, InputValue, PlanPrimitive};

use super::error::HarnessError;
use super::plan_view::{edges, primitive_word};
use super::source::Source;
use super::{kit, print_json, usage, Failure, Outcome};

/// Compiles every path; exit 1 when any is invalid, 2 when one is unreadable.
pub(super) fn validate(paths: &[PathBuf], json: bool) -> Outcome {
    if let Some(missing) = paths.iter().find(|p| !p.exists()) {
        return Err(usage(format!("{} does not exist", missing.display())));
    }
    let reports: Vec<_> = paths.iter().map(|p| validate_one(p)).collect();
    if json {
        print_json(&reports)?;
    } else {
        for report in &reports {
            if report.valid {
                let name = report.harnesses.last().map_or("", |h| h.name.as_str());
                println!("OK {} ({name})", report.path.display());
            } else {
                println!("INVALID {}", report.path.display());
                for error in &report.errors {
                    println!("  {error}");
                }
            }
        }
    }
    Ok(i32::from(reports.iter().any(|r| !r.valid)))
}

/// Prints what the harness at `path` is. An invalid harness is exit 1; a
/// path that cannot be read is exit 2.
pub(super) fn inspect(path: &Path, json: bool) -> Outcome {
    match inspect_report(path) {
        Ok(report) => {
            if json {
                print_json(&report)?;
            } else {
                print_report(&report);
            }
            Ok(0)
        }
        Err(error @ HarnessError::Harness(_)) => {
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
            let contract = io_contract(path).map_err(|e| match e {
                HarnessError::Harness(_) => Failure {
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

fn print_report(report: &InspectReport) {
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
}

/// A harness that compiled.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct ValidatedHarness {
    /// The file it came from.
    pub path: std::path::PathBuf,
    /// Its name.
    pub name: String,
    /// Its plan digest.
    pub plan_digest: String,
    /// Its bundle digest, when it is a bundle.
    pub bundle_digest: Option<String>,
}

/// The result of compiling a harness and its imports offline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct ValidateReport {
    /// The path that was validated.
    pub path: std::path::PathBuf,
    /// Whether everything compiled and every import resolves.
    pub valid: bool,
    /// Every problem found, each naming its file.
    pub errors: Vec<String>,
    /// The harnesses that compiled.
    pub harnesses: Vec<ValidatedHarness>,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct StepInfo {
    /// The step id.
    pub id: String,
    /// The primitive word.
    pub primitive: String,
    /// The steps it can continue to.
    pub successors: Vec<String>,
}

/// What a harness is, without running it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct InspectReport {
    /// The main document's file.
    pub path: std::path::PathBuf,
    /// The plan's name.
    pub name: String,
    /// The plan's digest.
    pub plan_digest: String,
    /// The bundle's digest, when it is a bundle.
    pub bundle_digest: Option<String>,
    /// The entry step.
    pub entry: String,
    /// Declared capabilities.
    pub capabilities: Vec<String>,
    /// Declared limits.
    pub limits: Option<sven_harness_spec::PlanLimits>,
    /// Declared inputs by name and type word.
    pub inputs: std::collections::BTreeMap<String, String>,
    /// Declared outputs by name and type word.
    pub outputs: std::collections::BTreeMap<String, String>,
    /// Every step, by id.
    pub steps: Vec<StepInfo>,
}

/// Compiles the harness at `path` and the workflows it imports, offline.
fn validate_one(path: &Path) -> ValidateReport {
    let mut report = ValidateReport {
        path: path.to_path_buf(),
        valid: false,
        errors: Vec::new(),
        harnesses: Vec::new(),
    };
    match Source::open(path) {
        Ok(source) => {
            report.harnesses.push(ValidatedHarness {
                path: source.path,
                name: source.plan.name,
                plan_digest: source.plan.digest,
                bundle_digest: source.bundle_digest,
            });
            report.valid = true;
        }
        Err(e) => report.errors.push(e.to_string()),
    }
    report
}

/// Describes the harness at `path`.
///
/// # Errors
///
/// [`HarnessError`] when it cannot be opened or compiled.
fn inspect_report(path: &Path) -> Result<InspectReport, HarnessError> {
    let main = Source::open(path)?;
    let plan = &main.plan;
    let steps = plan
        .steps
        .values()
        .map(|step| StepInfo {
            id: step.id.clone(),
            primitive: primitive_word(&step.primitive),
            successors: successors(&step.primitive),
        })
        .collect();
    Ok(InspectReport {
        path: main.path.clone(),
        name: plan.name.clone(),
        plan_digest: plan.digest.clone(),
        bundle_digest: main.bundle_digest.clone(),
        entry: plan.entry.clone(),
        capabilities: plan.capabilities.clone(),
        limits: plan.limits.clone(),
        inputs: type_words(plan.inputs.as_ref()),
        outputs: type_words(plan.outputs.as_ref()),
        steps,
    })
}

fn successors(primitive: &PlanPrimitive) -> Vec<String> {
    edges(primitive).into_iter().map(|e| e.to).collect()
}

fn type_words(declared: Option<&BTreeMap<String, InputValue>>) -> BTreeMap<String, String> {
    declared
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.clone(), type_word(&value.ty).to_string()))
        .collect()
}

fn type_word(ty: &IOType) -> &'static str {
    match ty {
        IOType::String => "string",
        IOType::Number => "number",
        IOType::Boolean => "boolean",
        IOType::Json => "json",
    }
}

/// The harness's input/output contract as JSON Schema fragments:
/// `{name, plan_digest, inputs, outputs}`.
///
/// # Errors
///
/// As [`inspect`].
fn io_contract(path: &Path) -> Result<Value, HarnessError> {
    let plan = Source::open(path)?.plan;
    Ok(json!({
        "name": plan.name,
        "plan_digest": plan.digest,
        "inputs": object_schema(plan.inputs.as_ref(), json!({"type": "object"})),
        "outputs": object_schema(plan.outputs.as_ref(), json!({})),
    }))
}

fn object_schema(declared: Option<&BTreeMap<String, InputValue>>, undeclared: Value) -> Value {
    let Some(fields) = declared else {
        return undeclared;
    };
    let mut properties = Map::new();
    for (name, field) in fields {
        let mut schema = Map::new();
        match field.ty {
            IOType::Json => {}
            ref ty => {
                schema.insert("type".into(), json!(type_word(ty)));
            }
        }
        if let Some(description) = &field.description {
            schema.insert("description".into(), json!(description));
        }
        properties.insert(name.clone(), Value::Object(schema));
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": fields.keys().collect::<Vec<_>>(),
        "additionalProperties": false,
    })
}
