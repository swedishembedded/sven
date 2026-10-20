// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Carrying `model.enable_thinking` to a brain server.

use crate::config::ModelConfig;

impl ModelConfig {
    /// This configuration with `enable_thinking` carried in `driver_options`
    /// as `chat_template_kwargs.enable_thinking`, where a brain server reads
    /// it. A value `driver_options` already holds is kept.
    #[must_use]
    pub fn with_thinking_in_driver_options(&self) -> Self {
        let mut cfg = self.clone();
        let Some(enabled) = self.enable_thinking else {
            return cfg;
        };
        if !cfg.driver_options.is_object() {
            cfg.driver_options = serde_json::json!({});
        }
        let kwargs = cfg
            .driver_options
            .as_object_mut()
            .expect("driver_options was made an object above")
            .entry("chat_template_kwargs")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(kwargs) = kwargs.as_object_mut() {
            kwargs
                .entry("enable_thinking")
                .or_insert(serde_json::Value::Bool(enabled));
        }
        cfg
    }
}

/// Carries a request's own thinking switch into `body`, where a server that
/// reads it (llama.cpp, vLLM, brain) finds it in `chat_template_kwargs`. It
/// beats the configured switch and `driver_options`. The key is not in the
/// OpenAI schema, which rejects unknown fields, so the `openai` driver never
/// calls this.
pub(crate) fn apply_request_switch(body: &mut serde_json::Value, enabled: Option<bool>) {
    let Some(enabled) = enabled else { return };
    if !body["chat_template_kwargs"].is_object() {
        body["chat_template_kwargs"] = serde_json::json!({});
    }
    body["chat_template_kwargs"]["enable_thinking"] = serde_json::json!(enabled);
}
