// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Type conversions between sven's tool types and rmcp's MCP model types.
//!
//! These are pure, stateless functions - no allocation beyond what the output
//! types require.  The bridge sits at the seam between the existing
//! `sven-tool-registry` crate and the MCP wire protocol so neither side needs to
//! know about the other.

use std::sync::Arc;

use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool as McpTool};
use sven_tool_registry::{ToolOutput, ToolOutputPart, ToolSchema};

/// Convert a [`ToolSchema`] (sven) into an rmcp [`McpTool`] descriptor.
///
/// The JSON Schema stored in [`ToolSchema::parameters`] is already valid
/// JSON Schema produced by each tool's [`sven_tool_registry::Tool::parameters_schema`]
/// implementation, so we pass it through as the `input_schema` without
/// further processing.
pub fn schema_to_mcp_tool(schema: ToolSchema) -> McpTool {
    let input_schema: JsonObject = value_to_object(schema.parameters);
    McpTool::new(
        std::borrow::Cow::Owned(schema.name),
        std::borrow::Cow::Owned(schema.description),
        Arc::new(input_schema),
    )
}

/// Build a [`JsonObject`] (serde_json::Map) from a raw JSON Schema value.
///
/// MCP requires the schema to be a JSON object; if the provided value is
/// already an object we use it directly, otherwise we wrap it in a minimal
/// `{"type":"object"}` envelope.
fn value_to_object(v: serde_json::Value) -> JsonObject {
    use serde_json::{Map, Value};
    match v {
        Value::Object(m) => m,
        other => {
            let mut m = Map::new();
            m.insert("type".to_string(), Value::String("object".to_string()));
            m.insert("value".to_string(), other);
            m
        }
    }
}

/// Convert a sven [`ToolOutput`] into an rmcp [`CallToolResult`].
///
/// Text parts become text blocks; image and audio parts (base64 data URIs)
/// become image and audio blocks with the MIME type taken from the data URI.
/// The MCP `is_error` flag mirrors sven's [`ToolOutput::is_error`].
pub fn output_to_call_result(output: ToolOutput) -> CallToolResult {
    let content: Vec<ContentBlock> = output
        .parts
        .into_iter()
        .map(|part| match part {
            ToolOutputPart::Text(t) => ContentBlock::text(t),
            // data URIs: `data:<mime>;base64,<b64>`
            ToolOutputPart::Image(data_uri) => {
                let (mime, data) = parse_data_uri(&data_uri);
                ContentBlock::image(data, mime)
            }
            ToolOutputPart::Audio(data_uri) => {
                let (mime, data) = parse_data_uri(&data_uri);
                ContentBlock::audio(data, mime)
            }
        })
        .collect();

    if output.is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// Split a data URI into its MIME type and base64 payload.
///
/// Falls back to `("application/octet-stream", whole_string)` when the URI
/// does not match the expected format so callers always receive a valid pair.
fn parse_data_uri(uri: &str) -> (&str, &str) {
    // Format: `data:<mime>;base64,<data>`
    if let Some(rest) = uri.strip_prefix("data:") {
        if let Some((mime_part, data)) = rest.split_once(';') {
            if let Some(b64) = data.strip_prefix("base64,") {
                return (mime_part, b64);
            }
        }
    }
    ("application/octet-stream", uri)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};
    use sven_tool_registry::{ToolOutput, ToolOutputPart, ToolSchema};

    use super::*;

    fn make_schema(name: &str, desc: &str, params: Value) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: desc.to_string(),
            parameters: params,
            is_mcp: false,
        }
    }

    // ── schema_to_mcp_tool ─────────────────────────────────────────────────

    #[test]
    fn schema_to_mcp_tool_preserves_name_and_description() {
        let schema = make_schema("read_file", "Reads a file", json!({"type":"object"}));
        let tool = schema_to_mcp_tool(schema);
        assert_eq!(tool.name.as_ref(), "read_file");
        assert_eq!(tool.description.as_deref(), Some("Reads a file"));
    }

    #[test]
    fn schema_to_mcp_tool_object_schema_passes_through() {
        let schema = make_schema(
            "grep",
            "Greps",
            json!({"type": "object", "properties": {"pattern": {"type": "string"}}}),
        );
        let tool = schema_to_mcp_tool(schema);
        // The "type" key should be preserved in the input_schema map.
        assert!(tool.input_schema.contains_key("type"));
    }

    #[test]
    fn schema_to_mcp_tool_non_object_schema_gets_wrapped() {
        let schema = make_schema("echo", "Echoes", json!("not an object"));
        let tool = schema_to_mcp_tool(schema);
        assert_eq!(
            tool.input_schema.get("type"),
            Some(&Value::String("object".to_string()))
        );
    }

    // ── output_to_call_result ──────────────────────────────────────────────

    #[test]
    fn output_to_call_result_text_success() {
        let out = ToolOutput::ok("id1", "hello world");
        let result = output_to_call_result(out);
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn output_to_call_result_error_flag_set() {
        let out = ToolOutput::err("id2", "something went wrong");
        let result = output_to_call_result(out);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content.len(), 1);
    }

    #[test]
    fn output_to_call_result_image_part_produces_content() {
        let out = ToolOutput::with_parts(
            "id3",
            vec![ToolOutputPart::Image(
                "data:image/png;base64,abc123".to_string(),
            )],
        );
        let result = output_to_call_result(out);
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 1);
    }

    /// MCP results carry audio blocks, so audio reaches the client as audio.
    #[test]
    fn output_to_call_result_audio_part_is_an_audio_block() {
        let out = ToolOutput::with_parts(
            "id5",
            vec![ToolOutputPart::Audio(
                "data:audio/wav;base64,UklG".to_string(),
            )],
        );
        let result = output_to_call_result(out);
        let audio = result.content[0].as_audio().expect("an audio block");
        assert_eq!(audio.mime_type, "audio/wav");
        assert_eq!(audio.data, "UklG");
    }

    #[test]
    fn output_to_call_result_mixed_parts_preserves_count() {
        let out = ToolOutput::with_parts(
            "id4",
            vec![
                ToolOutputPart::Text("prefix".to_string()),
                ToolOutputPart::Image("data:image/jpeg;base64,xyz".to_string()),
                ToolOutputPart::Text("suffix".to_string()),
            ],
        );
        let result = output_to_call_result(out);
        assert_eq!(result.content.len(), 3);
    }

    // ── parse_data_uri ─────────────────────────────────────────────────────

    #[test]
    fn parse_data_uri_valid() {
        let (mime, data) = parse_data_uri("data:image/png;base64,AAAA");
        assert_eq!(mime, "image/png");
        assert_eq!(data, "AAAA");
    }

    #[test]
    fn parse_data_uri_invalid_falls_back() {
        let uri = "not-a-data-uri";
        let (mime, data) = parse_data_uri(uri);
        assert_eq!(mime, "application/octet-stream");
        assert_eq!(data, uri);
    }

    #[test]
    fn parse_data_uri_jpeg() {
        let (mime, data) = parse_data_uri("data:image/jpeg;base64,/9j/4A==");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(data, "/9j/4A==");
    }
}
