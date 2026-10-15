// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that check what a model asks a
// tool to do before the tool does it, for its clients. If your team needs
// expertise in guarding tool calls you can procure our services by sending an
// email to info@swedishembedded.com.

//! A deterministic JSON Schema check of a tool call's arguments.
//!
//! The model proposes arguments; the tool declares a parameter schema. The
//! check compares the two and reports every way they disagree as a typed
//! [`ArgumentViolation`], in a fixed order, so the same call always yields
//! the same list. It only observes: the call still reaches the tool, which
//! answers a bad argument in its own words, and the violations go into the
//! trajectory beside the gate's decision.

use atif::ArgumentViolation;
use serde_json::Value;

/// Every way `args` breaks `schema`, ordered by path, rule and message.
///
/// A schema that is not itself a valid JSON Schema yields nothing: the check
/// cannot say the arguments are wrong against a yardstick that is broken.
pub(crate) fn argument_violations(schema: &Value, args: &Value) -> Vec<ArgumentViolation> {
    let Ok(validator) = jsonschema::validator_for(schema) else {
        tracing::debug!("tool parameter schema does not compile; arguments not checked");
        return Vec::new();
    };
    let mut violations: Vec<ArgumentViolation> = validator
        .iter_errors(args)
        .map(|error| ArgumentViolation {
            path: error.instance_path().to_string(),
            rule: error.kind().keyword().to_string(),
            message: error.to_string(),
        })
        .collect();
    violations.sort_by(|a, b| (&a.path, &a.rule, &a.message).cmp(&(&b.path, &b.rule, &b.message)));
    violations
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer", "minimum": 1}
            },
            "required": ["command"]
        })
    }

    #[test]
    fn arguments_that_satisfy_the_schema_have_no_violations() {
        assert!(argument_violations(&schema(), &json!({"command": "ls"})).is_empty());
    }

    #[test]
    fn a_missing_required_property_is_a_required_violation_at_the_root() {
        let found = argument_violations(&schema(), &json!({}));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "required");
        assert_eq!(found[0].path, "");
        assert!(found[0].message.contains("command"), "{found:?}");
    }

    #[test]
    fn a_wrongly_typed_property_names_its_pointer_and_the_type_rule() {
        let found = argument_violations(&schema(), &json!({"command": 7}));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].rule, "type");
        assert_eq!(found[0].path, "/command");
    }

    #[test]
    fn every_violation_is_reported_in_a_fixed_order() {
        let args = json!({"command": 7, "timeout": 0});
        let first = argument_violations(&schema(), &args);
        let rules: Vec<_> = first
            .iter()
            .map(|v| (v.path.as_str(), v.rule.as_str()))
            .collect();
        assert_eq!(rules, [("/command", "type"), ("/timeout", "minimum")]);
        assert_eq!(first, argument_violations(&schema(), &args));
    }

    #[test]
    fn a_schema_that_does_not_compile_checks_nothing() {
        assert!(argument_violations(&json!({"type": 12}), &json!({})).is_empty());
    }
}
