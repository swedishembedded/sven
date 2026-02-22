use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::debug;

use crate::{
    catalog::{static_catalog, ModelCatalogEntry},
    provider::ResponseStream,
    CompletionRequest, MessageContent, ResponseEvent, Role,
};

pub struct OpenAiProvider {
    model: String,
    api_key: Option<String>,
    base_url: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(
        model: String,
        api_key: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com".into()),
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.2),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl crate::ModelProvider for OpenAiProvider {
    fn name(&self) -> &str { "openai" }
    fn model_name(&self) -> &str { &self.model }

    /// List models by querying `GET /v1/models`, then enriching with static catalog metadata.
    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        let key = match &self.api_key {
            Some(k) => k.clone(),
            None => {
                // No key: return static catalog entries only.
                return Ok(static_catalog()
                    .into_iter()
                    .filter(|e| e.provider == "openai")
                    .collect());
            }
        };

        let resp = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .bearer_auth(&key)
            .send()
            .await
            .context("OpenAI list-models request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenAI list-models error {status}: {text}");
        }

        let body: Value = resp.json().await.context("OpenAI list-models parse failed")?;
        let catalog = static_catalog();

        let mut entries: Vec<ModelCatalogEntry> = Vec::new();
        if let Some(data) = body["data"].as_array() {
            for item in data {
                let id = match item["id"].as_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                // Enrich with static catalog metadata if available.
                if let Some(cat) = catalog.iter().find(|e| e.provider == "openai" && e.id == id) {
                    entries.push(cat.clone());
                } else {
                    // Unknown model — add a minimal entry.
                    entries.push(ModelCatalogEntry {
                        id: id.clone(),
                        name: id.clone(),
                        provider: "openai".into(),
                        context_window: 0,
                        max_output_tokens: 0,
                        description: String::new(),
                    });
                }
            }
        }

        // Sort by name for stable output.
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entries)
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let key = self.api_key.as_deref().context("OPENAI_API_KEY not set")?;

        let messages: Vec<Value> = req.messages.iter().map(|m| {
            match &m.content {
                MessageContent::Text(t) => json!({
                    "role": role_str(&m.role),
                    "content": t,
                }),
                MessageContent::ToolCall { tool_call_id, function } => json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "id": tool_call_id,
                        "type": "function",
                        "function": {
                            "name": function.name,
                            "arguments": function.arguments,
                        }
                    }]
                }),
                MessageContent::ToolResult { tool_call_id, content } => json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content,
                }),
            }
        }).collect();

        let tools: Vec<Value> = req.tools.iter().map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            }
        })).collect();

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": req.stream,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        debug!(model = %self.model, "sending completion request");

        let resp = self.client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .context("OpenAI request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenAI error {status}: {text}");
        }

        let byte_stream = resp.bytes_stream();
        let event_stream = byte_stream.flat_map(|chunk| {
            let lines = match chunk {
                Ok(b) => String::from_utf8_lossy(&b).to_string(),
                Err(e) => return futures::stream::iter(vec![Err(anyhow::anyhow!(e))]),
            };
            let events: Vec<anyhow::Result<ResponseEvent>> = lines
                .lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("data: ")?.trim();
                    if line == "[DONE]" {
                        return Some(Ok(ResponseEvent::Done));
                    }
                    let v: Value = serde_json::from_str(line).ok()?;
                    Some(parse_sse_chunk(&v))
                })
                .collect();
            futures::stream::iter(events)
        });

        Ok(Box::pin(event_stream))
    }
}

fn role_str(r: &Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn parse_sse_chunk(v: &Value) -> anyhow::Result<ResponseEvent> {
    // Usage-only chunk
    if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
        return Ok(ResponseEvent::Usage {
            input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
        });
    }

    let choice = &v["choices"][0];
    let delta = &choice["delta"];

    // Tool call delta
    if let Some(tool_calls) = delta.get("tool_calls") {
        if let Some(tc) = tool_calls.get(0) {
            let id = tc["id"].as_str().unwrap_or("").to_string();
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            let args = tc["function"]["arguments"].as_str().unwrap_or("").to_string();
            return Ok(ResponseEvent::ToolCall { id, name, arguments: args });
        }
    }

    // Text delta
    if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
        return Ok(ResponseEvent::TextDelta(text.to_string()));
    }

    Ok(ResponseEvent::TextDelta(String::new()))
}

