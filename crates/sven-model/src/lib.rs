mod types;
mod provider;
mod openai;
mod anthropic;
mod mock;
mod yaml_mock;

pub use types::*;
pub use provider::ModelProvider;
pub use openai::OpenAiProvider;
pub use anthropic::AnthropicProvider;
pub use mock::{MockProvider, ScriptedMockProvider};
pub use yaml_mock::YamlMockProvider;

use anyhow::bail;
use sven_config::ModelConfig;

/// Construct a boxed [`ModelProvider`] from configuration.
///
/// Provider selection:
/// - `"openai"` → [`OpenAiProvider`]
/// - `"anthropic"` → [`AnthropicProvider`]
/// - `"mock"` → [`YamlMockProvider`] if a responses file is configured,
///   otherwise [`MockProvider`] (echo-back)
pub fn from_config(cfg: &ModelConfig) -> anyhow::Result<Box<dyn ModelProvider>> {
    let key = resolve_api_key(cfg);
    match cfg.provider.as_str() {
        "openai" => Ok(Box::new(OpenAiProvider::new(
            cfg.name.clone(),
            key,
            cfg.base_url.clone(),
            cfg.max_tokens,
            cfg.temperature,
        ))),
        "anthropic" => Ok(Box::new(AnthropicProvider::new(
            cfg.name.clone(),
            key,
            cfg.base_url.clone(),
            cfg.max_tokens,
            cfg.temperature,
        ))),
        "mock" => {
            // Prefer env var, then config field
            let responses_path = std::env::var("SVEN_MOCK_RESPONSES").ok()
                .or_else(|| cfg.mock_responses_file.clone());
            if let Some(path) = responses_path {
                Ok(Box::new(YamlMockProvider::from_file(&path)?))
            } else {
                Ok(Box::new(MockProvider))
            }
        }
        other => bail!("unknown model provider: {other}"),
    }
}

fn resolve_api_key(cfg: &ModelConfig) -> Option<String> {
    if let Some(k) = &cfg.api_key {
        return Some(k.clone());
    }
    if let Some(env) = &cfg.api_key_env {
        return std::env::var(env).ok();
    }
    None
}
