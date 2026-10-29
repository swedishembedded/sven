// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for measuring how a prompt change
// alters an LLM agent's next decision, in minutes instead of whole benchmark
// runs, for its clients. If your team needs expertise in prompt and tool
// definition tuning for coding agents then you can procure our services by
// sending an email to info@swedishembedded.com.
//! One replayed sample as a JSON line: what the model decided.
//!
//! A decision is the tool calls it made (which tool, with which argument
//! names) and whether each call's arguments satisfy that tool's parameter
//! schema as the replayed request offered it. Counting these over many
//! samples of two variants is how a prompt change is compared.

use serde_json::{json, Value};
use sven_model::ToolSchema;
use sven_model_drivers::request_log::{RecordedCall, Response};

/// One tool call: its name, the argument names and whether the arguments
/// validate against the offered tool's schema.
fn call(call: &RecordedCall, tools: &[ToolSchema]) -> Value {
    let mut errors: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    match serde_json::from_str::<Value>(if call.arguments.trim().is_empty() {
        "{}"
    } else {
        &call.arguments
    }) {
        Err(e) => errors.push(format!("arguments are not JSON: {e}")),
        Ok(args) => {
            if let Some(object) = args.as_object() {
                names = object.keys().cloned().collect();
                names.sort();
            }
            match tools.iter().find(|t| t.name == call.name) {
                None => errors.push(format!("tool {:?} was not offered", call.name)),
                Some(tool) => match jsonschema::validator_for(&tool.parameters) {
                    Err(e) => errors.push(format!("the tool's schema does not compile: {e}")),
                    Ok(validator) => {
                        errors.extend(validator.iter_errors(&args).map(|e| e.to_string()));
                    }
                },
            }
        }
    }
    json!({
        "name": call.name,
        "arguments": names,
        "valid": errors.is_empty(),
        "errors": errors,
    })
}

/// The JSON line of sample `sample` of `variant` for request `request`.
/// `tools` are the tools the sent request offered.
pub(crate) fn line(
    variant: &str,
    request: usize,
    sample: u32,
    response: &Response,
    tools: &[ToolSchema],
) -> Value {
    json!({
        "variant": variant,
        "request": request,
        "sample": sample,
        "calls": response.tool_calls.iter().map(|c| call(c, tools)).collect::<Vec<_>>(),
        "text": response.text,
        "output_tokens": response.output_tokens,
        "reasoning_chars": response.reasoning.chars().count(),
        "error": response.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "grep".into(),
            description: String::new(),
            parameters: json!({
                "type": "object",
                "properties": {"pattern": {"type": "string"}},
                "required": ["pattern"]
            }),
            ..ToolSchema::default()
        }]
    }

    fn response(name: &str, arguments: &str) -> Response {
        Response {
            tool_calls: vec![RecordedCall {
                name: name.into(),
                arguments: arguments.into(),
                ..RecordedCall::default()
            }],
            output_tokens: 7,
            reasoning: "think".into(),
            ..Response::default()
        }
    }

    #[test]
    fn a_call_that_satisfies_the_schema_is_valid() {
        let l = line("v", 3, 2, &response("grep", r#"{"pattern":"x"}"#), &tools());
        assert_eq!(l["calls"][0]["arguments"], json!(["pattern"]));
        assert_eq!(l["calls"][0]["valid"], true);
        assert_eq!(
            (l["variant"].as_str(), l["sample"].as_u64()),
            (Some("v"), Some(2))
        );
        assert_eq!(
            (l["output_tokens"].as_u64(), l["reasoning_chars"].as_u64()),
            (Some(7), Some(5))
        );
    }

    #[test]
    fn a_missing_argument_an_unknown_tool_and_broken_json_are_invalid() {
        for (name, args) in [("grep", "{}"), ("nope", "{}"), ("grep", "{\"pattern\"")] {
            let l = line("v", 1, 1, &response(name, args), &tools());
            assert_eq!(l["calls"][0]["valid"], false, "{name} {args}");
            assert!(!l["calls"][0]["errors"].as_array().unwrap().is_empty());
        }
    }
}
