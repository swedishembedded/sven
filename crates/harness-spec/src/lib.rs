// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The sven harness language: YAML agent-harness documents, their typed
//! AST, static validation and the compiler to a canonical, content-hashed
//! [`HarnessPlan`].
//!
//! A harness document is a *program*: a map of steps, each step one of a
//! closed set of primitives, compiled ahead of run time into a plan whose
//! digest pins exactly what will execute. This crate knows nothing about
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
mod error;
mod expr;
mod plan;
mod spec;
mod validate;

#[cfg(test)]
mod tests;

pub use bundle::{CompiledBundle, HarnessBundle, Helper, BUNDLE_FORMAT_VERSION};
pub use error::HarnessError;
pub use expr::{Expr, Path, Template};
pub use plan::{HarnessPlan, PlanLimits, PlanPrimitive, PlanStep, PLAN_FORMAT_VERSION};
pub use spec::{
    AgentStepSpec, AssignStepSpec, BranchStepSpec, CapabilityName, DelegateStepSpec,
    DelegateTarget, FailStepSpec, HarnessSpec, IOType, InputValue, LimitsSpec, ParallelStepSpec,
    PredictStepSpec, RetryStepSpec, ReturnStepSpec, StepId, StepSpec, ToolStepSpec,
    TransformStepSpec, VerifyStepSpec, SUPPORTED_API_VERSION,
};

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

/// Formats a typed-deserialization failure. The message already carries the
/// field path when the deserializer knows it; prefix it so the reader can
/// tell parse-shape errors from validation errors.
fn parse_error_with_path(err: &serde_json::Error) -> String {
    format!("typed spec: {err}")
}

/// A parsed, ownership-taken harness document not yet compiled.
#[derive(Debug, Clone)]
pub struct HarnessDocument {
    spec: HarnessSpec,
}

impl HarnessDocument {
    /// Parses `yaml` into a typed harness specification.
    ///
    /// # Errors
    ///
    /// YAML syntax errors and any shape the deserializer cannot fit into
    /// [`HarnessSpec`] (unknown primitive, wrong field types, missing
    /// required fields).
    pub fn parse(yaml: &str) -> Result<Self, HarnessError> {
        // serde_yaml's untagged-enum handling loses structural errors; going
        // through serde_json::Value (a lossless round-trip for the JSON-ish
        // subset the language uses) keeps the typed deserialization exact.
        let raw: serde_yaml::Value =
            serde_yaml::from_str(yaml).map_err(|e| HarnessError::Parse(e.to_string()))?;
        let value = yaml_to_json(&raw)?;
        let spec: HarnessSpec = serde_json::from_value(value)
            .map_err(|e| HarnessError::Parse(parse_error_with_path(&e)))?;
        Ok(Self { spec })
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
    /// unsupported `api_version`, missing or dangling step references,
    /// unbounded cycles, undeclared capabilities, missing limits where the
    /// primitive set can run away, and output-contract mismatches.
    pub fn compile(&self) -> Result<HarnessPlan, HarnessError> {
        validate::validate(&self.spec)?;
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
