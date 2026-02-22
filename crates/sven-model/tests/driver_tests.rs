//! Shared test harness for integration-testing model drivers against live APIs.
//!
//! All tests in this module are `#[ignore]`d by default so they do not run
//! in CI without network access and real API keys.  To run them:
//!
//! ```sh
//! # Run all integration tests:
//! OPENAI_API_KEY=sk-... cargo test -p sven-model -- --include-ignored
//!
//! # Run tests for a specific provider:
//! GROQ_API_KEY=gsk_... cargo test -p sven-model groq -- --include-ignored
//! ```

use futures::StreamExt;
use sven_config::ModelConfig;
use sven_model::{from_config, CompletionRequest, Message, ResponseEvent};

// ── Shared harness ────────────────────────────────────────────────────────────

pub struct DriverTestHarness {
    provider_id: &'static str,
    test_model: &'static str,
    api_key_env: &'static str,
}

impl DriverTestHarness {
    pub fn new(
        provider_id: &'static str,
        test_model: &'static str,
        api_key_env: &'static str,
    ) -> Self {
        Self { provider_id, test_model, api_key_env }
    }

    fn make_config(&self) -> ModelConfig {
        ModelConfig {
            provider: self.provider_id.into(),
            name: self.test_model.into(),
            api_key_env: Some(self.api_key_env.into()),
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
        };

        let mut stream = provider.complete(req).await?;
        let mut text = String::new();
        let mut got_done = false;

        while let Some(ev) = stream.next().await {
            match ev? {
                ResponseEvent::TextDelta(t) => text.push_str(&t),
                ResponseEvent::Done => { got_done = true; break; }
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
        assert!(!models.is_empty(), "list_models should return at least one entry");
        Ok(())
    }
}

// ── Per-provider tests ────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY and network"]
async fn test_openai_basic() {
    let h = DriverTestHarness::new("openai", "gpt-4o-mini", "OPENAI_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY and network"]
async fn test_openai_list_models() {
    let h = DriverTestHarness::new("openai", "gpt-4o-mini", "OPENAI_API_KEY");
    h.test_list_models().await.unwrap();
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY and network"]
async fn test_anthropic_basic() {
    let h = DriverTestHarness::new("anthropic", "claude-3-haiku-20240307", "ANTHROPIC_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY and network"]
async fn test_google_basic() {
    let h = DriverTestHarness::new("google", "gemini-1.5-flash-002", "GEMINI_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY and network"]
async fn test_aws_bedrock_basic() {
    let h = DriverTestHarness::new("aws", "amazon.nova-micro-v1:0", "AWS_ACCESS_KEY_ID");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires COHERE_API_KEY and network"]
async fn test_cohere_basic() {
    let h = DriverTestHarness::new("cohere", "command-r", "COHERE_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires GROQ_API_KEY and network"]
async fn test_groq_basic() {
    let h = DriverTestHarness::new("groq", "llama-3.1-8b-instant", "GROQ_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires TOGETHER_API_KEY and network"]
async fn test_together_basic() {
    let h = DriverTestHarness::new(
        "together",
        "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
        "TOGETHER_API_KEY",
    );
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires MISTRAL_API_KEY and network"]
async fn test_mistral_basic() {
    let h = DriverTestHarness::new("mistral", "mistral-small-latest", "MISTRAL_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY and network"]
async fn test_deepseek_basic() {
    let h = DriverTestHarness::new("deepseek", "deepseek-chat", "DEEPSEEK_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires XAI_API_KEY and network"]
async fn test_xai_basic() {
    let h = DriverTestHarness::new("xai", "grok-beta", "XAI_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires PERPLEXITY_API_KEY and network"]
async fn test_perplexity_basic() {
    let h = DriverTestHarness::new("perplexity", "sonar", "PERPLEXITY_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY and network"]
async fn test_openrouter_basic() {
    let h = DriverTestHarness::new(
        "openrouter",
        "meta-llama/llama-3.3-70b-instruct",
        "OPENROUTER_API_KEY",
    );
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires running Ollama server at localhost:11434"]
async fn test_ollama_basic() {
    let h = DriverTestHarness::new("ollama", "llama3.2", "IGNORED_NO_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires DASHSCOPE_API_KEY and network"]
async fn test_dashscope_basic() {
    let h = DriverTestHarness::new("dashscope", "qwen-turbo", "DASHSCOPE_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires MOONSHOT_API_KEY and network"]
async fn test_moonshot_basic() {
    let h = DriverTestHarness::new("moonshot", "moonshot-v1-8k", "MOONSHOT_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires CEREBRAS_API_KEY and network"]
async fn test_cerebras_basic() {
    let h = DriverTestHarness::new("cerebras", "llama3.1-8b", "CEREBRAS_API_KEY");
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires FIREWORKS_API_KEY and network"]
async fn test_fireworks_basic() {
    let h = DriverTestHarness::new(
        "fireworks",
        "accounts/fireworks/models/llama-v3p3-70b-instruct",
        "FIREWORKS_API_KEY",
    );
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires NVIDIA_API_KEY and network"]
async fn test_nvidia_basic() {
    let h = DriverTestHarness::new(
        "nvidia",
        "meta/llama-3.3-70b-instruct",
        "NVIDIA_API_KEY",
    );
    h.test_basic_completion().await.unwrap();
}

#[tokio::test]
#[ignore = "requires SAMBANOVA_API_KEY and network"]
async fn test_sambanova_basic() {
    let h = DriverTestHarness::new(
        "sambanova",
        "Meta-Llama-3.3-70B-Instruct",
        "SAMBANOVA_API_KEY",
    );
    h.test_basic_completion().await.unwrap();
}
