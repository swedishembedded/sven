// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Run variables: typed values a workflow declares, that agents read and set
//! through the tools the harness answers itself ([`VAR_TOOLS`]) and that
//! command gates may capture, each task seeing only the variables it names.
//!
//! A variable's value lives in the run's facts under [`var_fact`], with
//! [`var_set_fact`] recording that it was set, so it is checkpointed,
//! recorded and replayed like every other part of the run. This module holds
//! the declarations, the access a step is granted and the check a value must
//! pass before it is stored; the machine does the storing.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::steps::IOType;

pub use crate::names::VAR_TOOLS;

/// The most bytes one variable's value may take, as JSON text: values are
/// shown to agents and carried in every checkpoint.
pub const MAX_VAR_BYTES: usize = 256 * 1024;

/// A declared variable, as it travels in the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableDecl {
    /// The type every value must have.
    #[serde(rename = "type")]
    pub ty: IOType,
    /// For a `json` variable, the shape its value must have (a JSON Schema
    /// subset: `type`, `properties`, `required`, `items`, `enum`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    /// What the variable is for, as an agent is shown it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// What an agent step may do with a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarAccess {
    /// Read it with `harness_vars_get`.
    Read,
    /// Read and set it.
    Write,
}

/// The fact a variable's value is kept in.
#[must_use]
pub fn var_fact(name: &str) -> String {
    format!("var_{name}")
}

/// The fact that records a variable was set (a value may itself be `null`
/// or `false`, so the value cannot say it).
#[must_use]
pub fn var_set_fact(name: &str) -> String {
    format!("var_set_{name}")
}

/// Checks `value` against `decl` and returns the value to store: a `json`
/// variable given a string that is itself a JSON object or array stores the
/// parsed value (models often send structured values as text).
///
/// # Errors
///
/// Why the value does not fit, written for the agent that sent it.
pub fn admit(decl: &VariableDecl, value: Value) -> Result<Value, String> {
    let value = match (&decl.ty, value) {
        (IOType::Json, Value::String(text)) => match serde_json::from_str::<Value>(&text) {
            Ok(parsed @ (Value::Object(_) | Value::Array(_))) => parsed,
            _ => Value::String(text),
        },
        (_, value) => value,
    };
    let fits = match decl.ty {
        IOType::String => value.is_string(),
        IOType::Number => value.is_number(),
        IOType::Boolean => value.is_boolean(),
        IOType::Json => true,
    };
    if !fits {
        return Err(format!(
            "the value must be a {} (got {})",
            io_name(&decl.ty),
            kind(&value)
        ));
    }
    if let Some(schema) = &decl.schema {
        conforms(schema, &value, "value")?;
    }
    let bytes = value.to_string().len();
    if bytes > MAX_VAR_BYTES {
        return Err(format!(
            "the value is {bytes} bytes; at most {MAX_VAR_BYTES} are allowed"
        ));
    }
    Ok(value)
}

fn io_name(ty: &IOType) -> &'static str {
    match ty {
        IOType::String => "string",
        IOType::Number => "number",
        IOType::Boolean => "boolean",
        IOType::Json => "JSON value",
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Whether `value` has the shape `schema` describes, for the subset of JSON
/// Schema a variable declaration may use.
///
/// # Errors
///
/// The first place where it does not, as a path from `at`.
pub fn conforms(schema: &Value, value: &Value, at: &str) -> Result<(), String> {
    if let Some(want) = schema.get("type").and_then(Value::as_str) {
        let ok = match want {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !ok {
            return Err(format!("{at} must be {want} (got {})", kind(value)));
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.contains(value) {
            return Err(format!(
                "{at} must be one of {}",
                Value::Array(options.clone())
            ));
        }
    }
    if let Value::Object(map) = value {
        for key in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !map.contains_key(key) {
                return Err(format!("{at} lacks the required field `{key}`"));
            }
        }
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (key, sub) in props {
                if let Some(field) = map.get(key) {
                    conforms(sub, field, &format!("{at}.{key}"))?;
                }
            }
        }
    }
    if let (Value::Array(items), Some(sub)) = (value, schema.get("items")) {
        for (i, item) in items.iter().enumerate() {
            conforms(sub, item, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl(ty: IOType, schema: Option<Value>) -> VariableDecl {
        VariableDecl {
            ty,
            schema,
            description: None,
        }
    }

    #[test]
    fn a_value_is_admitted_only_with_its_declared_type_and_shape() {
        let checks = decl(
            IOType::Json,
            Some(json!({ "type": "object", "required": ["test"],
                         "properties": { "test": { "type": "string" } } })),
        );
        assert_eq!(
            admit(&checks, json!({ "test": "cargo test" })),
            Ok(json!({ "test": "cargo test" }))
        );
        // Sent as text, stored as the structure it spells.
        assert_eq!(
            admit(&checks, json!("{\"test\": \"cargo test\"}")),
            Ok(json!({ "test": "cargo test" }))
        );
        assert!(admit(&checks, json!({})).unwrap_err().contains("`test`"));
        assert!(admit(&checks, json!({ "test": 3 }))
            .unwrap_err()
            .contains("value.test must be string"));
        assert!(admit(&decl(IOType::String, None), json!(3)).is_err());
        assert!(admit(&decl(IOType::Boolean, None), json!(false)).is_ok());
        let big = "x".repeat(MAX_VAR_BYTES + 1);
        assert!(admit(&decl(IOType::String, None), json!(big)).is_err());
    }
}
