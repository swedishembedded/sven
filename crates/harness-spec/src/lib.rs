// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The sven harness language: YAML workflow documents, their typed AST,
//! static validation and the compiler to a canonical, content-hashed
//! [`HarnessPlan`].
//!
//! A harness document declares *what a job is*: tasks, the trusted gates that
//! decide whether each succeeded, how failures are repaired and what counts as
//! success. It is lowered ahead of run time into a plan - a map of steps, each
//! one of a closed set of primitives - whose digest pins exactly what will
//! execute. This crate knows nothing about
//! models, tools or the kernel — it is pure data plus a compiler, with no
//! I/O of its own, so an entire harness can be validated offline
//! (`sven harness validate`) and its digest checked before anything runs.
//!
//! The execution of a compiled plan lives in `sven-harness-runtime`, which
//! interprets the plan through the HSM kernel's existing effects, events and
//! permission planes (ADR 0004). Nothing in this crate depends on any other
//! sven crate: the wall is what keeps the language reviewable as data.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

mod bundle;
mod decoding;
mod error;
mod expr;
mod gate_command;
mod gate_steps;
mod iterate;
mod lower;
mod lower_body;
mod lower_checks;
mod lower_context;
mod lower_corrections;
mod lower_counts;
mod lower_imports;
mod lower_system;
mod names;
pub mod package;
mod parse;
mod plan;
mod rewrite;
mod scratchpad;
mod source_map;
mod steps;
mod steps_validate;
#[cfg(feature = "testing")]
pub mod testing;
pub mod vars;
mod workflow;
mod workflow_validate;
mod workflow_vars;

#[cfg(test)]
mod tests;

pub use bundle::{CompiledBundle, HarnessBundle, BUNDLE_FORMAT_VERSION};
pub use error::HarnessError;
pub use expr::{Expr, Path, PathRewrite, Template};
pub use gate_command::GateCommand;
pub use plan::{
    HarnessPlan, PlanGateStep, PlanIteration, PlanLimits, PlanPrimitive, PlanStep,
    PLAN_FORMAT_VERSION,
};
pub use source_map::{Lowering, SourceRef};
pub use steps::{
    AgentStepSpec, AssignStepSpec, BranchStepSpec, CapabilityName, DelegateStepSpec, FailStepSpec,
    ForEachSpec, GateStepSpec, HarnessSpec, IOType, InputValue, IterateSource, IterateStepSpec,
    JsonSource, LimitsSpec, PredictStepSpec, Reconcile, ReturnStepSpec, StepDecoding, StepId,
    StepSpec, UntilSpec, VerifyStepSpec, API_VERSION, DEFAULT_GATE_TIMEOUT_SECS, DEFAULT_MAX_ITEMS,
    DIGEST_TOOL, GATE_TOOL, ITEM_FAILED_FACT, MAX_COVERS, MAX_GATE_TIMEOUT_SECS, MAX_ITEMS_LIMIT,
    MAX_ITERATIONS_LIMIT, RESERVED_TOOLS, SNAPSHOT_TOOL,
};
pub use workflow::Imported;

/// The JSON Schema (draft 2020-12) of a harness document, for editors and
/// `sven harness schema`: the declarative workflow language of ADR 0005. It
/// forbids unknown fields everywhere; references, bounds and capabilities are
/// checked by the compiler.
pub const DOCUMENT_SCHEMA: &str = include_str!("../schema/harness-document.schema.json");

/// Converts a [`serde_yaml::Value`] to a [`serde_json::Value`]. The harness
/// language uses the JSON-representable subset of YAML; tagged values,
/// non-string keys and anything else outside it are a parse error naming
/// the shape, not a silent reinterpretation.
fn yaml_to_json(value: &serde_yaml::Value) -> Result<serde_json::Value, HarnessError> {
    Ok(match value {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        // A YAML number's text is a JSON number's text for the language's
        // subset; parse through serde_json so integer/float handling is
        // serde_json's, not ours. A number outside serde_json's range
        // degrades to the string form — visible in any template it lands
        // in, never silently truncated.
        serde_yaml::Value::Number(n) => {
            match serde_json::from_str::<serde_json::Number>(&n.to_string()) {
                Ok(number) => serde_json::Value::Number(number),
                Err(_) => serde_json::Value::String(n.to_string()),
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(yaml_to_json(item)?);
            }
            serde_json::Value::Array(out)
        }
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                let Some(key_text) = key.as_str() else {
                    return Err(HarnessError::Parse(format!(
                        "harness map keys must be strings, got {key:?}"
                    )));
                };
                out.insert(key_text.to_string(), yaml_to_json(item)?);
            }
            serde_json::Value::Object(out)
        }
        serde_yaml::Value::Tagged(tagged) => {
            return Err(HarnessError::Parse(format!(
                "harness documents use no YAML tags: !{}",
                tagged.tag
            )));
        }
    })
}

/// Checks the `api_version` a raw document declares.
fn check_api_version(value: &serde_json::Value) -> Result<(), HarnessError> {
    match value.get("api_version") {
        Some(serde_json::Value::String(version)) if version == API_VERSION => Ok(()),
        Some(serde_json::Value::String(version)) if version == "1" => {
            Err(HarnessError::Invalid(format!(
                "`api_version: \"1\"` documents (the step-map language) are no longer accepted; \
                 write a workflow with `api_version: \"{API_VERSION}\"`"
            )))
        }
        Some(serde_json::Value::String(version)) => Err(HarnessError::Invalid(format!(
            "unsupported api_version `{version}` (this compiler supports `{API_VERSION}`)"
        ))),
        Some(other) => Err(HarnessError::Parse(format!(
            "`api_version` must be a quoted string such as \"{API_VERSION}\", got {other}"
        ))),
        None => Err(HarnessError::Parse(format!(
            "the document has no `api_version` (write `api_version: \"{API_VERSION}\"`)"
        ))),
    }
}

/// A parsed, ownership-taken harness document not yet compiled.
#[derive(Debug, Clone)]
pub struct HarnessDocument {
    spec: HarnessSpec,
}

impl HarnessDocument {
    /// Parses a workflow document and lowers it into the step specification
    /// the compiler works on.
    ///
    /// # Errors
    ///
    /// YAML syntax errors, a missing or unsupported `api_version`, any shape
    /// the workflow deserializer cannot fit (unknown key, wrong field types,
    /// missing required fields), and the workflow validation and lowering
    /// errors.
    pub fn parse(yaml: &str) -> Result<Self, HarnessError> {
        Self::parse_in(yaml, std::collections::BTreeMap::new())
    }

    /// Like [`Self::parse`], for a document of a package: `resolved` holds
    /// the interface of each import, by alias (see [`package`]).
    ///
    /// # Errors
    ///
    /// As [`Self::parse`].
    pub fn parse_in(
        yaml: &str,
        resolved: std::collections::BTreeMap<String, Imported>,
    ) -> Result<Self, HarnessError> {
        // serde_yaml's untagged-enum handling loses structural errors; going
        // through serde_json::Value (a lossless round-trip for the JSON-ish
        // subset the language uses) keeps the typed deserialization exact.
        let raw: serde_yaml::Value =
            serde_yaml::from_str(yaml).map_err(|e| HarnessError::Parse(e.to_string()))?;
        let value = yaml_to_json(&raw)?;
        check_api_version(&value)?;
        Ok(Self {
            spec: parse::lower_document_with(value, resolved)?,
        })
    }

    /// The `imports` of a document, alias to path as written.
    ///
    /// # Errors
    ///
    /// YAML syntax errors, an unsupported `api_version`, or `imports` that is
    /// not a map of strings.
    pub fn imports_of(
        yaml: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, HarnessError> {
        let raw: serde_yaml::Value =
            serde_yaml::from_str(yaml).map_err(|e| HarnessError::Parse(e.to_string()))?;
        let value = yaml_to_json(&raw)?;
        check_api_version(&value)?;
        parse::imports_of(&value)
    }

    /// Parses a harness document from YAML bytes.
    ///
    /// # Errors
    ///
    /// As [`HarnessDocument::parse`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, HarnessError> {
        Self::parse(std::str::from_utf8(bytes)?)
    }

    /// The typed specification this document parsed into.
    #[must_use]
    pub fn spec(&self) -> &HarnessSpec {
        &self.spec
    }

    /// Validates and compiles the document into a canonical [`HarnessPlan`].
    ///
    /// # Errors
    ///
    /// Every static-validation failure listed under [`HarnessError::Invalid`]:
    /// missing or dangling step references, unbounded cycles, undeclared
    /// capabilities, missing limits where the primitive set can run away, and
    /// output-contract mismatches.
    pub fn compile(&self) -> Result<HarnessPlan, HarnessError> {
        steps_validate::validate(&self.spec)?;
        Ok(plan::compile(&self.spec))
    }
}

/// Convenience: parse and compile `yaml` in one call.
///
/// # Errors
///
/// As [`HarnessDocument::parse`] then [`HarnessDocument::compile`].
pub fn compile_str(yaml: &str) -> Result<HarnessPlan, HarnessError> {
    HarnessDocument::parse(yaml)?.compile()
}
