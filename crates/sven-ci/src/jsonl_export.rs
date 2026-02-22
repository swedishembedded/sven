// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! JSONL trace export for fine-tuning datasets.
//!
//! This module converts sven's internal message format to various fine-tuning
//! formats expected by different model providers.

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde_json::{json, Value};
use sven_model::{ContentPart, Message, MessageContent, Role, ToolContentPart, ToolResultContent};

/// Output format for JSONL export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonlFormat {
    /// OpenAI fine-tuning format with `tool_calls` array and `tool` role.
    /// Compatible with: OpenAI, Azure OpenAI, most OpenAI-compatible providers.
    #[default]
    OpenAI,
    /// Anthropic format with user/assistant blocks (not yet implemented).
    /// Compatible with: Anthropic Claude models.
    Anthropic,
    /// Raw sven internal format (preserves exact structure).
    /// Use for debugging or custom processing pipelines.
    Raw,
}

/// Write the complete conversation trace to a JSONL file.
///
/// Each line is a JSON object representing a message in the specified format.
/// System messages are INCLUDED so the file contains the complete trace
/// suitable for fine-tuning datasets.
///
/// # Arguments
/// * `path` - Output file path (will be created/truncated)
/// * `messages` - Complete conversation including system message
/// * `format` - Target format for the output
///
/// # Errors
/// Returns an error if file creation or writing fails.
pub fn write_jsonl_trace(
    path: &Path,
    messages: &[Message],
    format: JsonlFormat,
) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(path)
        .with_context(|| format!("creating JSONL file {}", path.display()))?;
    
    let converted = match format {
        JsonlFormat::OpenAI => convert_to_openai_format(messages),
        JsonlFormat::Anthropic => convert_to_anthropic_format(messages)?,
        JsonlFormat::Raw => convert_to_raw_format(messages),
    };
    
    for msg_json in converted {
        let json = serde_json::to_string(&msg_json)
            .context("serializing message to JSON")?;
        writeln!(file, "{}", json)
            .with_context(|| format!("writing to JSONL file {}", path.display()))?;
    }
    
    Ok(())
}

/// Convert messages to OpenAI fine-tuning format.
///
/// This format uses:
/// - `role`: "system" | "user" | "assistant" | "tool"
/// - `content`: text string or array of content parts
/// - `tool_calls`: array for assistant tool invocations
/// - `tool_call_id`: for tool results
fn convert_to_openai_format(messages: &[Message]) -> Vec<Value> {
    messages.iter().map(|m| {
        match &m.content {
            MessageContent::Text(t) => json!({
                "role": role_to_str(&m.role),
                "content": t,
            }),
            
            MessageContent::ContentParts(parts) if !parts.is_empty() => {
                let content: Vec<Value> = parts.iter().map(|p| match p {
                    ContentPart::Text { text } => {
                        json!({ "type": "text", "text": text })
                    }
                    ContentPart::Image { image_url, detail } => {
                        let mut img_obj = json!({ "url": image_url });
                        if let Some(d) = detail {
                            img_obj.as_object_mut().unwrap().insert("detail".to_string(), json!(d));
                        }
                        json!({ "type": "image_url", "image_url": img_obj })
                    }
                }).collect();
                json!({
                    "role": role_to_str(&m.role),
                    "content": content,
                })
            }
            
            MessageContent::ContentParts(_) => {
                // Empty parts fallback
                json!({
                    "role": role_to_str(&m.role),
                    "content": "",
                })
            }
            
            MessageContent::ToolCall { tool_call_id, function } => {
                json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": tool_call_id,
                        "type": "function",
                        "function": {
                            "name": function.name,
                            "arguments": function.arguments,
                        }
                    }]
                })
            }
            
            MessageContent::ToolResult { tool_call_id, content } => {
                let wire_content: Value = match content {
                    ToolResultContent::Text(t) => json!(t),
                    ToolResultContent::Parts(parts) if !parts.is_empty() => {
                        let arr: Vec<Value> = parts.iter().map(|p| match p {
                            ToolContentPart::Text { text } => {
                                json!({ "type": "text", "text": text })
                            }
                            ToolContentPart::Image { image_url } => {
                                json!({
                                    "type": "image_url",
                                    "image_url": { "url": image_url },
                                })
                            }
                        }).collect();
                        json!(arr)
                    }
                    ToolResultContent::Parts(_) => json!(""),
                };
                json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": wire_content,
                })
            }
        }
    }).collect()
}

/// Convert messages to Anthropic fine-tuning format.
///
/// Note: Anthropic uses a different structure with content blocks.
/// This implementation provides a basic conversion; full Anthropic format
/// support may require additional fields.
fn convert_to_anthropic_format(messages: &[Message]) -> anyhow::Result<Vec<Value>> {
    // Anthropic separates system message from conversation messages
    let mut result = Vec::new();
    
    for msg in messages {
        match &msg.role {
            Role::System => {
                // Anthropic expects system as separate parameter, not in messages array
                // For fine-tuning JSONL, we'll include it with a special marker
                result.push(json!({
                    "role": "system",
                    "content": match &msg.content {
                        MessageContent::Text(t) => t.clone(),
                        _ => "".to_string(),
                    }
                }));
            }
            Role::User => {
                let content = match &msg.content {
                    MessageContent::Text(t) => vec![json!({"type": "text", "text": t})],
                    MessageContent::ContentParts(parts) => {
                        parts.iter().map(|p| match p {
                            ContentPart::Text { text } => json!({"type": "text", "text": text}),
                            ContentPart::Image { image_url, .. } => json!({
                                "type": "image",
                                "source": {"type": "base64", "data": image_url}
                            }),
                        }).collect()
                    }
                    _ => vec![json!({"type": "text", "text": ""})],
                };
                result.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
            Role::Assistant => {
                match &msg.content {
                    MessageContent::Text(t) => {
                        result.push(json!({
                            "role": "assistant",
                            "content": [{"type": "text", "text": t}],
                        }));
                    }
                    MessageContent::ToolCall { tool_call_id, function } => {
                        // Parse arguments back to JSON for Anthropic's input field
                        let input: Value = serde_json::from_str(&function.arguments)
                            .unwrap_or(json!({}));
                        result.push(json!({
                            "role": "assistant",
                            "content": [{
                                "type": "tool_use",
                                "id": tool_call_id,
                                "name": function.name,
                                "input": input,
                            }],
                        }));
                    }
                    _ => {
                        result.push(json!({
                            "role": "assistant",
                            "content": [{"type": "text", "text": ""}],
                        }));
                    }
                }
            }
            Role::Tool => {
                if let MessageContent::ToolResult { tool_call_id, content } = &msg.content {
                    let text_content = match content {
                        ToolResultContent::Text(t) => t.clone(),
                        ToolResultContent::Parts(parts) => {
                            parts.iter()
                                .filter_map(|p| match p {
                                    ToolContentPart::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    };
                    result.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": text_content,
                        }],
                    }));
                }
            }
        }
    }
    
    Ok(result)
}

/// Convert messages to raw internal format.
///
/// This uses sven's native Message serialization without any transformation.
/// Useful for debugging or custom processing pipelines.
fn convert_to_raw_format(messages: &[Message]) -> Vec<Value> {
    messages.iter().map(|m| {
        // Use serde to convert to Value
        serde_json::to_value(m).unwrap_or(json!({"error": "serialization failed"}))
    }).collect()
}

/// Convert Role enum to string representation.
fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::{FunctionCall, Message};

    #[test]
    fn test_openai_format_simple_text() {
        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];
        
        let result = convert_to_openai_format(&messages);
        assert_eq!(result.len(), 3);
        
        assert_eq!(result[0]["role"], "system");
        assert_eq!(result[0]["content"], "You are a helpful assistant.");
        
        assert_eq!(result[1]["role"], "user");
        assert_eq!(result[1]["content"], "Hello");
        
        assert_eq!(result[2]["role"], "assistant");
        assert_eq!(result[2]["content"], "Hi there!");
    }
    
    #[test]
    fn test_openai_format_tool_call() {
        let tool_call = Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "call_123".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"/test.txt"}"#.into(),
                },
            },
        };
        
        let result = convert_to_openai_format(&[tool_call]);
        assert_eq!(result.len(), 1);
        
        assert_eq!(result[0]["role"], "assistant");
        assert_eq!(result[0]["content"], Value::Null);
        assert!(result[0]["tool_calls"].is_array());
        
        let calls = result[0]["tool_calls"].as_array().unwrap();
        assert_eq!(calls[0]["id"], "call_123");
        assert_eq!(calls[0]["type"], "function");
        assert_eq!(calls[0]["function"]["name"], "read_file");
    }
    
    #[test]
    fn test_openai_format_tool_result() {
        let tool_result = Message::tool_result("call_123", "file contents here");
        
        let result = convert_to_openai_format(&[tool_result]);
        assert_eq!(result.len(), 1);
        
        assert_eq!(result[0]["role"], "tool");
        assert_eq!(result[0]["tool_call_id"], "call_123");
        assert_eq!(result[0]["content"], "file contents here");
    }
    
    #[test]
    fn test_anthropic_format_basic() {
        let messages = vec![
            Message::system("System prompt"),
            Message::user("User question"),
            Message::assistant("Assistant response"),
        ];
        
        let result = convert_to_anthropic_format(&messages).unwrap();
        assert_eq!(result.len(), 3);
        
        // Anthropic includes system separately
        assert_eq!(result[0]["role"], "system");
        assert_eq!(result[1]["role"], "user");
        assert_eq!(result[2]["role"], "assistant");
    }
    
    #[test]
    fn test_raw_format_preserves_structure() {
        let messages = vec![
            Message::user("test"),
        ];
        
        let result = convert_to_raw_format(&messages);
        assert_eq!(result.len(), 1);
        assert!(result[0].is_object());
        assert_eq!(result[0]["role"], "user");
    }
}
