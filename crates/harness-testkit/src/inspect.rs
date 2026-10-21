// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Offline validation, inspection and the input/output contract of a harness.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in auditing what
//! an agent workflow may do before it runs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Map, Value};
use sven_harness_spec::{HarnessPlan, IOType, InputValue, PlanPrimitive};

use crate::dir::HarnessDir;
use crate::error::TestkitError;
use crate::source::Source;
use crate::view::{edges, primitive_word};

/// A harness that compiled.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidatedHarness {
    /// The file it came from.
    pub path: std::path::PathBuf,
    /// Its name.
    pub name: String,
    /// Its plan digest.
    pub plan_digest: String,
    /// Its bundle digest, when it is a bundle.
    pub bundle_digest: Option<String>,
}

/// The result of compiling a harness and its children offline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidateReport {
    /// The path that was validated.
    pub path: std::path::PathBuf,
    /// Whether everything compiled and every delegate resolves.
    pub valid: bool,
    /// Every problem found, each naming its file.
    pub errors: Vec<String>,
    /// The harnesses that compiled, children first.
    pub harnesses: Vec<ValidatedHarness>,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepInfo {
    /// The step id.
    pub id: String,
    /// The primitive word.
    pub primitive: String,
    /// The steps it can continue to.
    pub successors: Vec<String>,
}

/// What a harness is, without running it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InspectReport {
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
    /// Case names found beside it.
    pub cases: Vec<String>,
}

/// Compiles the harness at `path` and the workflows it imports, offline.
#[must_use]
pub fn validate(path: &Path) -> ValidateReport {
    match HarnessDir::open(path) {
        Ok(dir) => validate_dir(&dir, path),
        Err(e) => ValidateReport {
            path: path.to_path_buf(),
            valid: false,
            errors: vec![e.to_string()],
            harnesses: Vec::new(),
        },
    }
}

/// As [`validate`], for a directory that is already open.
pub(crate) fn validate_dir(dir: &HarnessDir, path: &Path) -> ValidateReport {
    let mut report = ValidateReport {
        path: path.to_path_buf(),
        valid: false,
        errors: Vec::new(),
        harnesses: Vec::new(),
    };
    match Source::read(&dir.main) {
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

/// The compiled plan of the main document in `dir`, which reads a run's trace
/// in the author's terms.
///
/// # Errors
///
/// [`TestkitError`] when the document cannot be read or compiled.
pub fn main_plan(dir: &HarnessDir) -> Result<HarnessPlan, TestkitError> {
    Ok(Source::read(&dir.main)?.plan)
}

/// Describes the harness at `path`.
///
/// # Errors
///
/// [`TestkitError`] when it cannot be opened or compiled.
pub fn inspect(path: &Path) -> Result<InspectReport, TestkitError> {
    let dir = HarnessDir::open(path)?;
    let main = Source::read(&dir.main)?;
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
    let cases = dir
        .cases
        .iter()
        .filter_map(|file| file.file_stem().map(|s| s.to_string_lossy().into_owned()))
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
        cases,
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
pub fn io_schema(path: &Path) -> Result<Value, TestkitError> {
    let dir = HarnessDir::open(path)?;
    let plan = Source::read(&dir.main)?.plan;
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
