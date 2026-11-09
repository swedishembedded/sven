// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that run image, speech,
// vision and other models as their user for its clients. If your team needs
// expertise in multimodal model APIs then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Running any model once through a service's invocation API: list what it
//! serves (`GET {base}/models`), run one (`POST {base}/invocations`), read a
//! file it made (`GET {base}/files/{id}/content`).
//!
//! What a model makes stays with the service and comes back as a handle
//! (`mcp-file://files/<id>`); bytes are fetched only when asked for. A request
//! may name the conversation it belongs to (`X-Sven-Session`), so the files
//! are that conversation's, and carries an idempotency key, so a repeat after
//! a restart answers the same files and is charged once.
//!
//! Every answer is checked: a handle is one this client can name again, a
//! name is a plain file name, sizes and texts are bounded.

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde_json::Value;

/// How long a listing may take.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one invocation may take (an image at a high resolution can take
/// minutes).
const INVOKE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long a download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// The most text an answer's `text` carries into a tool result.
const MAX_TEXT_CHARS: usize = 200_000;
/// The most models a listing is read for.
const MAX_MODELS: usize = 5_000;

/// The header naming the conversation an invocation belongs to.
pub const SESSION_HEADER: &str = "x-sven-session";
/// The scheme of a handle the service keeps.
pub const HANDLE_PREFIX: &str = "mcp-file://files/";

/// One model a service lists, with what it reads and makes.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedModel {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// What it reads (`text`, `image`, `audio`, `file`, `video`).
    pub input: Vec<String>,
    /// What it makes (`text`, `image`, `speech`, `audio`, `video`,
    /// `transcription`, `embeddings`).
    pub output: Vec<String>,
    /// Its price lines as listed (`prompt`, `completion`, `image`, ...).
    pub pricing: Vec<(String, String)>,
    /// The voices of a speech model.
    pub voices: Vec<String>,
    /// Its parameters as listed: a list of names (chat) or descriptors by
    /// name (an image model's enums and ranges).
    pub parameters: Value,
}

impl ListedModel {
    /// The operations an invocation of this model can name, from what it
    /// reads and makes: `chat` for text out, `image`, `speech`, `audio`
    /// (music and sound from chat), `video`, `transcribe`.
    #[must_use]
    pub fn operations(&self) -> Vec<&'static str> {
        let makes = |kind: &str| self.output.iter().any(|o| o == kind);
        let mut ops = Vec::new();
        if makes("image") {
            ops.push("image");
        }
        if makes("speech") {
            ops.push("speech");
        }
        if makes("transcription") {
            ops.push("transcribe");
        }
        if makes("video") {
            ops.push("video");
        }
        if makes("audio") {
            ops.push("audio");
        }
        if makes("text") && !makes("image") && !makes("audio") {
            ops.push("chat");
        }
        ops
    }
}

/// One file a model made, by handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MadeFile {
    pub uri: String,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
}

/// What one invocation answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Invoked {
    pub id: String,
    pub text: Option<String>,
    pub outputs: Vec<MadeFile>,
    /// What it was charged, in micro-USD, when the service says.
    pub cost_micro_usd: Option<u64>,
}

/// A service's invocation API at its OpenAI-style base URL (ending in `/v1`).
#[derive(Debug, Clone)]
pub struct InvocationClient {
    base_url: String,
    api_key: String,
    session: Option<String>,
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.chars().take(200).collect())
                .collect()
        })
        .unwrap_or_default()
}

fn text(value: &Value, max: usize) -> Option<String> {
    value.as_str().map(|s| s.chars().take(max).collect())
}

/// One entry of a `/models` listing, or none when it has no usable id.
fn listed(entry: &Value) -> Option<ListedModel> {
    let id = entry["id"].as_str()?;
    if id.is_empty() || id.len() > 200 || id.chars().any(char::is_control) {
        return None;
    }
    let pricing = entry["pricing"]
        .as_object()
        .map(|p| {
            p.iter()
                .filter_map(|(k, v)| {
                    let shown = match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => return None,
                    };
                    Some((
                        k.chars().take(40).collect(),
                        shown.chars().take(40).collect(),
                    ))
                })
                .take(32)
                .collect()
        })
        .unwrap_or_default();
    Some(ListedModel {
        id: id.to_owned(),
        name: text(&entry["name"], 200),
        description: text(&entry["description"], 1_000),
        input: strings(&entry["architecture"]["input_modalities"]),
        output: strings(&entry["architecture"]["output_modalities"]),
        pricing,
        voices: strings(&entry["supported_voices"])
            .into_iter()
            .take(500)
            .collect(),
        parameters: entry["supported_parameters"].clone(),
    })
}

/// A handle the service answered, if it is one this client can use again.
fn handle(value: &Value) -> anyhow::Result<MadeFile> {
    let uri = value["uri"]
        .as_str()
        .ok_or_else(|| anyhow!("an output has no uri"))?;
    let id = uri.strip_prefix(HANDLE_PREFIX).ok_or_else(|| {
        anyhow!(
            "an output is not a file of the service: {}",
            uri.chars().take(60).collect::<String>()
        )
    })?;
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        bail!("an output names a file whose id cannot be used");
    }
    let name = value["name"].as_str().unwrap_or("output");
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let mime_type = value["mimeType"]
        .as_str()
        .unwrap_or("application/octet-stream");
    if mime_type.len() > 100 || !mime_type.contains('/') {
        bail!("an output has no usable type");
    }
    Ok(MadeFile {
        uri: uri.to_owned(),
        name: if name.trim_matches('.').is_empty() {
            "output".into()
        } else {
            name
        },
        mime_type: mime_type.to_owned(),
        size: value["size"].as_u64().unwrap_or(0),
    })
}

impl InvocationClient {
    /// `session` names the conversation outputs belong to; none makes them
    /// the key's own, kept for a while to be fetched.
    #[must_use]
    pub fn new(base_url: &str, api_key: &str, session: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            session,
        }
    }

    /// Whether outputs belong to a conversation of the service.
    #[must_use]
    pub fn in_conversation(&self) -> bool {
        self.session.is_some()
    }

    fn get(&self, url: String) -> reqwest::RequestBuilder {
        let request = crate::build_http_client()
            .get(url)
            .bearer_auth(&self.api_key);
        match &self.session {
            Some(session) => request.header(SESSION_HEADER, session),
            None => request,
        }
    }

    async fn read(response: reqwest::Response, what: &str) -> anyhow::Result<Value> {
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("{what}: reading the answer"))?;
        let json: Option<Value> = serde_json::from_str(&body).ok();
        if !status.is_success() {
            let message = json
                .as_ref()
                .and_then(|j| {
                    j["error"]["message"]
                        .as_str()
                        .or_else(|| j["error"].as_str())
                })
                .map_or_else(|| body.chars().take(200).collect(), str::to_owned);
            bail!("{what}: HTTP {}: {message}", status.as_u16());
        }
        json.ok_or_else(|| anyhow!("{what}: the answer is not JSON"))
    }

    /// The models the service lists.
    ///
    /// # Errors
    /// The service cannot be reached or refuses.
    pub async fn models(&self) -> anyhow::Result<Vec<ListedModel>> {
        let response = self
            .get(format!("{}/models", self.base_url))
            .timeout(LIST_TIMEOUT)
            .send()
            .await
            .context("listing the models")?;
        let answer = Self::read(response, "listing the models").await?;
        Ok(answer["data"]
            .as_array()
            .map(|data| data.iter().take(MAX_MODELS).filter_map(listed).collect())
            .unwrap_or_default())
    }

    /// Run one invocation (`request` is the invocation's JSON body) under
    /// `idempotency_key`.
    ///
    /// # Errors
    /// The service cannot be reached, refuses (its message is kept), or
    /// answers outputs that are not files of its own.
    pub async fn invoke(&self, request: &Value, idempotency_key: &str) -> anyhow::Result<Invoked> {
        let mut post = crate::build_http_client()
            .post(format!("{}/invocations", self.base_url))
            .bearer_auth(&self.api_key)
            .header("idempotency-key", idempotency_key)
            .timeout(INVOKE_TIMEOUT)
            .json(request);
        if let Some(session) = &self.session {
            post = post.header(SESSION_HEADER, session);
        }
        let response = post.send().await.context("running the model")?;
        let answer = Self::read(response, "running the model").await?;
        let outputs = answer["outputs"]
            .as_array()
            .map(|o| {
                o.iter()
                    .take(64)
                    .map(handle)
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Invoked {
            id: text(&answer["id"], 100).unwrap_or_default(),
            text: text(&answer["text"], MAX_TEXT_CHARS),
            outputs,
            cost_micro_usd: answer["usage"]["cost_micro_usd"]
                .as_str()
                .and_then(|c| c.parse().ok()),
        })
    }

    /// The bytes of file `uri` and its type, at most `max_bytes` of them.
    ///
    /// # Errors
    /// Not a handle of the service, unreachable, refused, or larger than
    /// `max_bytes`.
    pub async fn download(&self, uri: &str, max_bytes: u64) -> anyhow::Result<(Vec<u8>, String)> {
        let file =
            handle(&serde_json::json!({ "uri": uri, "mimeType": "application/octet-stream" }))?;
        let id = &file.uri[HANDLE_PREFIX.len()..];
        let mut response = self
            .get(format!("{}/files/{id}/content", self.base_url))
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .context("fetching the file")?;
        if !response.status().is_success() {
            // A refusal is read for its message; it never answers bytes.
            Self::read(response, "fetching the file").await?;
            bail!("fetching the file: the service refused");
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map_or_else(
                || "application/octet-stream".to_owned(),
                |v| v.split(';').next().unwrap_or(v).trim().to_owned(),
            );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.context("fetching the file")? {
            if (bytes.len() + chunk.len()) as u64 > max_bytes {
                bail!("the file is larger than {max_bytes} bytes");
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, mime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_listing_says_what_each_model_reads_makes_and_can_be_run_as() {
        let image = listed(&json!({
            "id": "google/gemini-nano-banana-2.1",
            "architecture": { "input_modalities": ["image", "text"], "output_modalities": ["image", "text"] },
            "pricing": { "prompt": "0.0000015", "image": 0.04 },
            "supported_parameters": { "resolution": { "type": "enum", "values": ["1K", "2K"] } }
        }))
        .unwrap();
        assert_eq!(image.operations(), ["image"]);
        assert_eq!(
            image.pricing,
            vec![
                ("image".into(), "0.04".into()),
                ("prompt".into(), "0.0000015".into())
            ]
        );
        let speech = listed(&json!({
            "id": "openai/tts", "architecture": { "output_modalities": ["speech"] }, "supported_voices": ["alloy"]
        }))
        .unwrap();
        assert_eq!(
            (speech.operations(), speech.voices),
            (vec!["speech"], vec!["alloy".to_owned()])
        );
        let vision = listed(&json!({ "id": "v", "architecture": { "input_modalities": ["text", "image"], "output_modalities": ["text"] } })).unwrap();
        assert_eq!(vision.operations(), ["chat"]);
        assert!(listed(&json!({ "id": "" })).is_none());
    }

    #[test]
    fn only_files_of_the_service_come_back_as_handles() {
        let made = handle(&json!({ "uri": "mcp-file://files/7b1d6c1e-2f0a", "name": "../image 1.png", "mimeType": "image/png", "size": 9 })).unwrap();
        assert_eq!((made.name.as_str(), made.size), (".._image_1.png", 9));
        assert!(
            handle(&json!({ "uri": "https://elsewhere/x.png", "mimeType": "image/png" })).is_err()
        );
        assert!(
            handle(&json!({ "uri": "mcp-file://files/../../etc", "mimeType": "image/png" }))
                .is_err()
        );
    }
}
