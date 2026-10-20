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
use sven_harness_spec::{DelegateTarget, HarnessPlan, IOType, InputValue, PlanPrimitive};

use crate::dir::HarnessDir;
use crate::error::TestkitError;
use crate::source::Source;

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

/// A bundled helper.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HelperInfo {
    /// The helper's name.
    pub name: String,
    /// Its ABI version.
    pub abi: String,
    /// Its language.
    pub language: String,
    /// Decoded size in bytes.
    pub bytes: usize,
}

/// A child harness.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChildInfo {
    /// The file.
    pub path: std::path::PathBuf,
    /// The child's name.
    pub name: String,
    /// The child's plan digest.
    pub plan_digest: String,
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
    /// Bundled helpers.
    pub helpers: Vec<HelperInfo>,
    /// Child harnesses found beside it.
    pub children: Vec<ChildInfo>,
    /// Case names found beside it.
    pub cases: Vec<String>,
}

/// Compiles the harness at `path` and its children offline.
///
/// Every file is compiled, and every `delegate` to a harness must name one
/// that is in the directory (or the harness itself).
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
    let mut sources = Vec::new();
    for file in dir.children.iter().chain(std::iter::once(&dir.main)) {
        match Source::read(file) {
            Ok(source) => sources.push(source),
            Err(e) => report.errors.push(e.to_string()),
        }
    }
    let names: Vec<&str> = sources.iter().map(|s| s.plan.name.as_str()).collect();
    for (index, source) in sources.iter().enumerate() {
        if names[..index].contains(&source.plan.name.as_str()) {
            report.errors.push(format!(
                "{}: another harness in this directory is already named `{}`",
                source.path.display(),
                source.plan.name
            ));
        }
        for target in harness_delegates(&source.plan) {
            if !names.contains(&target.1.as_str()) {
                report.errors.push(format!(
                    "{}: step `{}` delegates to harness `{}`, which is not among this directory's harnesses",
                    source.path.display(),
                    target.0,
                    target.1
                ));
            }
        }
    }
    report.harnesses = sources
        .into_iter()
        .map(|s| ValidatedHarness {
            path: s.path,
            name: s.plan.name,
            plan_digest: s.plan.digest,
            bundle_digest: s.bundle_digest,
        })
        .collect();
    report.valid = report.errors.is_empty();
    report
}

/// `(step, harness name)` for each delegate to a harness.
fn harness_delegates(plan: &HarnessPlan) -> Vec<(String, String)> {
    plan.steps
        .values()
        .filter_map(|step| match &step.primitive {
            PlanPrimitive::Delegate {
                target,
                as_kind: DelegateTarget::Harness,
                ..
            } => Some((step.id.clone(), target.clone())),
            _ => None,
        })
        .collect()
}

/// Describes the harness at `path`.
///
/// # Errors
///
/// [`TestkitError`] when it cannot be opened or compiled.
pub fn inspect(path: &Path) -> Result<InspectReport, TestkitError> {
    let dir = HarnessDir::open(path)?;
    let main = Source::read(&dir.main)?;
    let children = dir
        .children
        .iter()
        .map(|file| {
            Source::read(file).map(|s| ChildInfo {
                path: s.path,
                name: s.plan.name,
                plan_digest: s.plan.digest,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
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
    let helpers = main
        .bundle
        .iter()
        .flat_map(|bundle| bundle.helpers.iter())
        .map(|(name, helper)| HelperInfo {
            name: name.clone(),
            abi: helper.abi.clone(),
            language: helper.language.clone(),
            bytes: base64_len(&helper.content),
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
        helpers,
        children,
        cases,
    })
}

fn primitive_word(primitive: &PlanPrimitive) -> String {
    serde_json::to_value(primitive)
        .ok()
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

fn successors(primitive: &PlanPrimitive) -> Vec<String> {
    use PlanPrimitive::{
        Agent, Assign, Branch, Delegate, Fail, Gate, Parallel, Predict, Retry, Return, Tool,
        Transform, Verify,
    };
    match primitive {
        Predict { then, .. }
        | Agent { then, .. }
        | Tool { then, .. }
        | Verify { then, .. }
        | Assign { then, .. }
        | Delegate { then, .. }
        | Transform { then, .. } => vec![then.clone()],
        Branch {
            when_true,
            when_false,
            ..
        } => vec![when_true.clone(), when_false.clone()],
        Retry {
            body,
            then,
            on_exhausted,
            ..
        } => vec![body.clone(), then.clone(), on_exhausted.clone()],
        Parallel { branches, join } => branches
            .iter()
            .cloned()
            .chain(std::iter::once(join.clone()))
            .collect(),
        Gate { then, on_fail, .. } => vec![then.clone(), on_fail.clone()],
        Return { .. } | Fail { .. } => Vec::new(),
    }
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

/// Decoded size of base64 text, without decoding it.
fn base64_len(content: &str) -> usize {
    let text: Vec<u8> = content
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let padding = text.iter().rev().take_while(|b| **b == b'=').count();
    (text.len() / 4 * 3).saturating_sub(padding)
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
