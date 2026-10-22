// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared test harness for integration-testing model drivers against live APIs.
//!
//! All tests in this module are `#[ignore]`d by default so they do not run
//! in CI without network access and real API keys. Each uses the key stored
//! for its provider. To run them:
//!
//! ```sh
//! # Store the key of each provider to test, once:
//! sven auth set openai
//!
//! # Run all integration tests:
//! cargo test -p sven-model-drivers --test driver_tests -- --include-ignored
//!
//! # Run tests for a specific provider:
//! cargo test -p sven-model-drivers --test driver_tests groq -- --include-ignored
//! ```

use futures::StreamExt;
use sven_model::{CompletionRequest, Message, ResponseEvent, ToolSchema};
use sven_model_drivers::from_config;
use sven_model_drivers::ModelConfig;

// ── Shared harness ────────────────────────────────────────────────────────────

pub struct DriverTestHarness {
    provider_id: &'static str,
    test_model: &'static str,
}

impl DriverTestHarness {
    pub fn new(provider_id: &'static str, test_model: &'static str) -> Self {
        Self {
            provider_id,
            test_model,
        }
    }

    fn make_config(&self) -> ModelConfig {
        ModelConfig {
            provider: self.provider_id.into(),
            name: self.test_model.into(),
            max_tokens: Some(64),
            temperature: Some(0.0),
            ..ModelConfig::default()
        }
    }

    pub async fn test_basic_completion(&self) -> anyhow::Result<()> {
        let cfg = self.make_config();
        let provider = from_config(&cfg)?;

        let req = CompletionRequest {
            messages: vec![Message::user("Reply with exactly: 'hello'")],
            tools: vec![],
            stream: true,
            ..Default::default()
        };

        let mut stream = provider.complete(req).await?;
        let mut text = String::new();
        let mut got_done = false;

        while let Some(ev) = stream.next().await {
            match ev? {
                ResponseEvent::TextDelta(t) => text.push_str(&t),
                ResponseEvent::Done => {
                    got_done = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(got_done, "stream must end with Done event");
        assert!(!text.is_empty(), "model must produce some text");
        Ok(())
    }

    pub async fn test_list_models(&self) -> anyhow::Result<()> {
        let cfg = self.make_config();
        let provider = from_config(&cfg)?;
        let models = provider.list_models().await?;
        assert!(
            !models.is_empty(),
            "list_models should return at least one entry"
        );
        Ok(())
    }

    /// Verify that the provider correctly returns streamed chunks across at
    /// least two `TextDelta` events (i.e. real SSE streaming is working).
    pub async fn test_streaming_chunks(&self) -> anyhow::Result<()> {
        let cfg = self.make_config();
        let provider = from_config(&cfg)?;

        let req = CompletionRequest {
            messages: vec![Message::user(
                "Count from 1 to 5, one number per word, no extra text.",
            )],
            tools: vec![],
            stream: true,
            ..Default::default()
        };

        let mut stream = provider.complete(req).await?;
        let mut chunk_count = 0usize;
        let mut text = String::new();
        let mut got_done = false;

        while let Some(ev) = stream.next().await {
            match ev? {
                ResponseEvent::TextDelta(t) if !t.is_empty() => {
                    chunk_count += 1;
                    text.push_str(&t);
                }
                ResponseEvent::Done => {
                    got_done = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(got_done, "stream must end with Done");
        assert!(!text.is_empty(), "model must produce non-empty text");
        assert!(
            chunk_count >= 2,
            "expected at least 2 streaming text chunks (got {chunk_count}) - \
             verify SSE streaming is actually enabled"
        );
        Ok(())
    }

    /// Verify that the provider emits a `ToolCall` event when the model is
    /// given a tool that is directly relevant to the user message.
    ///
    /// The schema intentionally includes `additionalProperties` (common in sven
    /// built-in tools) and a nested object with its own `additionalProperties`
    /// so that any regression in Gemini schema sanitization is caught here.
    pub async fn test_tool_calling(&self) -> anyhow::Result<()> {
        let cfg = self.make_config();
        let provider = from_config(&cfg)?;

        let tool = ToolSchema {
            name: "get_current_time".into(),
            description: "Returns the current UTC time as an ISO-8601 string.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "timezone": {
                        "type": "string",
                        "description": "Optional IANA timezone name, e.g. 'Europe/Stockholm'"
                    },
                    "metadata": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Optional key-value metadata"
                    }
                },
                "required": [],
            }),
            is_mcp: false,
        };

        let req = CompletionRequest {
            messages: vec![Message::user(
                "What is the current time? You MUST use the get_current_time tool.",
            )],
            tools: vec![tool],
            stream: true,
            ..Default::default()
        };

        let mut stream = provider.complete(req).await?;
        let mut tool_calls: Vec<(String, String)> = vec![];
        let mut got_done = false;

        while let Some(ev) = stream.next().await {
            match ev? {
                ResponseEvent::ToolCall {
                    name, arguments, ..
                } if !name.is_empty() => {
                    tool_calls.push((name, arguments));
                }
                ResponseEvent::Done => {
                    got_done = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(got_done, "stream must end with Done");
        assert!(
            !tool_calls.is_empty(),
            "model must emit at least one ToolCall event when asked to use a tool"
        );
        let (name, _) = &tool_calls[0];
        assert_eq!(
            name, "get_current_time",
            "model should have called 'get_current_time', got '{name}'"
        );
        Ok(())
    }
}

// ── Per-provider tests ────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires a stored openai key (`sven auth set openai`) and network"]
async fn test_openai_basic() {
    let h = DriverTestHarness::new("openai", "gpt-4o-mini");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored openai key (`sven auth set openai`) and network"]
async fn test_openai_list_models() {
    let h = DriverTestHarness::new("openai", "gpt-4o-mini");
    h.test_list_models().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored anthropic key (`sven auth set anthropic`) and network"]
async fn test_anthropic_basic() {
    let h = DriverTestHarness::new("anthropic", "claude-3-haiku-20240307");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored google key (`sven auth set google`) and network"]
async fn test_google_basic() {
    let h = DriverTestHarness::new("google", "gemini-1.5-flash-002");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY and network"]
async fn test_aws_bedrock_basic() {
    let h = DriverTestHarness::new("aws", "amazon.nova-micro-v1:0");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored cohere key (`sven auth set cohere`) and network"]
async fn test_cohere_basic() {
    let h = DriverTestHarness::new("cohere", "command-r");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored groq key (`sven auth set groq`) and network"]
async fn test_groq_basic() {
    let h = DriverTestHarness::new("groq", "llama-3.1-8b-instant");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored together key (`sven auth set together`) and network"]
async fn test_together_basic() {
    let h = DriverTestHarness::new("together", "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored mistral key (`sven auth set mistral`) and network"]
async fn test_mistral_basic() {
    let h = DriverTestHarness::new("mistral", "mistral-small-latest");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored deepseek key (`sven auth set deepseek`) and network"]
async fn test_deepseek_basic() {
    let h = DriverTestHarness::new("deepseek", "deepseek-chat");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored xai key (`sven auth set xai`) and network"]
async fn test_xai_basic() {
    let h = DriverTestHarness::new("xai", "grok-beta");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored perplexity key (`sven auth set perplexity`) and network"]
async fn test_perplexity_basic() {
    let h = DriverTestHarness::new("perplexity", "sonar");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored openrouter key (`sven auth set openrouter`) and network"]
async fn test_openrouter_basic() {
    let h = DriverTestHarness::new("openrouter", "meta-llama/llama-3.3-70b-instruct");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires running Ollama server at localhost:11434"]
async fn test_ollama_basic() {
    let h = DriverTestHarness::new("ollama", "llama3.2");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored dashscope key (`sven auth set dashscope`) and network"]
async fn test_dashscope_basic() {
    let h = DriverTestHarness::new("dashscope", "qwen-turbo");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored moonshot key (`sven auth set moonshot`) and network"]
async fn test_moonshot_basic() {
    let h = DriverTestHarness::new("moonshot", "moonshot-v1-8k");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored cerebras key (`sven auth set cerebras`) and network"]
async fn test_cerebras_basic() {
    let h = DriverTestHarness::new("cerebras", "llama3.1-8b");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored fireworks key (`sven auth set fireworks`) and network"]
async fn test_fireworks_basic() {
    let h = DriverTestHarness::new(
        "fireworks",
        "accounts/fireworks/models/llama-v3p3-70b-instruct",
    );
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored nvidia key (`sven auth set nvidia`) and network"]
async fn test_nvidia_basic() {
    let h = DriverTestHarness::new("nvidia", "meta/llama-3.3-70b-instruct");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a stored sambanova key (`sven auth set sambanova`) and network"]
async fn test_sambanova_basic() {
    let h = DriverTestHarness::new("sambanova", "Meta-Llama-3.3-70B-Instruct");
    h.test_basic_completion().await.unwrap();
}

// ── Streaming chunk validation ─────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires a stored openai key (`sven auth set openai`) and network"]
async fn test_openai_streaming_chunks() {
    DriverTestHarness::new("openai", "gpt-4o-mini")
        .test_streaming_chunks()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored anthropic key (`sven auth set anthropic`) and network"]
async fn test_anthropic_streaming_chunks() {
    DriverTestHarness::new("anthropic", "claude-3-haiku-20240307")
        .test_streaming_chunks()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored google key (`sven auth set google`) and network"]
async fn test_google_streaming_chunks() {
    DriverTestHarness::new("google", "gemini-1.5-flash-002")
        .test_streaming_chunks()
        .await
        .unwrap();
}

// ── Tool calling ───────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires a stored openai key (`sven auth set openai`) and network"]
async fn test_openai_tool_calling() {
    DriverTestHarness::new("openai", "gpt-4o-mini")
        .test_tool_calling()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored anthropic key (`sven auth set anthropic`) and network"]
async fn test_anthropic_tool_calling() {
    DriverTestHarness::new("anthropic", "claude-3-haiku-20240307")
        .test_tool_calling()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored google key (`sven auth set google`) and network"]
async fn test_google_tool_calling() {
    DriverTestHarness::new("google", "gemini-1.5-flash-002")
        .test_tool_calling()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored groq key (`sven auth set groq`) and network"]
async fn test_groq_tool_calling() {
    DriverTestHarness::new("groq", "llama-3.3-70b-versatile")
        .test_tool_calling()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored cohere key (`sven auth set cohere`) and network"]
async fn test_cohere_tool_calling() {
    DriverTestHarness::new("cohere", "command-r-plus")
        .test_tool_calling()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored mistral key (`sven auth set mistral`) and network"]
async fn test_mistral_tool_calling() {
    DriverTestHarness::new("mistral", "mistral-small-latest")
        .test_tool_calling()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a stored deepseek key (`sven auth set deepseek`) and network"]
async fn test_deepseek_tool_calling() {
    DriverTestHarness::new("deepseek", "deepseek-chat")
        .test_tool_calling()
        .await
        .unwrap();
}

#[test]
fn a_key_file_that_does_not_exist_is_a_missing_key_naming_it() {
    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-sonnet-4-6".into(),
        api_key_file: Some("/nonexistent/sven-test-key".into()),
        ..ModelConfig::default()
    };
    let err = from_config(&cfg).err().expect("no key must be refused");
    let text = err.to_string();
    assert!(text.contains("/nonexistent/sven-test-key"), "{text}");
}

/// A key file the configuration names is the operator's statement that the
/// key is there, so its absence is an error even for a provider that needs
/// no key: silently running without it would hide the mistake.
#[test]
fn a_named_key_file_that_is_missing_is_refused_even_where_no_key_is_needed() {
    let cfg = ModelConfig {
        provider: "ollama".into(),
        name: "m".into(),
        api_key_file: Some("/nonexistent/sven-test-key".into()),
        ..ModelConfig::default()
    };
    assert!(from_config(&cfg).is_err());
    let keyless = ModelConfig {
        api_key_file: None,
        ..cfg
    };
    assert!(from_config(&keyless).is_ok());
}
