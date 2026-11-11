// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that combine image,
// speech, vision, music and video models for its clients. If your team needs
// expertise in multimodal agent tooling then you can procure our services by
// sending an email to info@swedishembedded.com.
//! `run_model`: one model run once, through the sven service
//! ([`super::invocation`]), a provider of its own ([`super::direct`]), or as
//! a durable video job ([`super::generate`]).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{ArtifactRef, Tool, ToolCall, ToolOutput};

use super::{direct, generate, invocation, invoker, MediaConfig, MediaEndpoint};

pub struct RunModelTool {
    endpoint: MediaEndpoint,
    config: MediaConfig,
    workdir: PathBuf,
}

impl RunModelTool {
    #[must_use]
    pub fn new(endpoint: MediaEndpoint, config: MediaConfig, workdir: PathBuf) -> Self {
        Self {
            endpoint,
            config,
            workdir,
        }
    }
}

impl RunModelTool {
    /// A video: a durable job written to a file ([`generate`]), its
    /// parameters as the video API names them.
    async fn video(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> Result<(String, Option<ArtifactRef>), String> {
        let mut video = match args.get("params") {
            None => serde_json::Map::new(),
            Some(Value::Object(params)) => params.clone(),
            Some(_) => return Err("`params` is an object of the operation's parameters".into()),
        };
        for key in ["model", "prompt", "output"] {
            if let Some(value) = args.get(key) {
                video.insert(key.into(), value.clone());
            }
        }
        if let Some(inputs) = args.get("inputs").and_then(Value::as_array) {
            let references = inputs
                .iter()
                .map(|input| {
                    let path = input.as_str().ok_or("`inputs` holds paths")?;
                    if path.starts_with(sven_model_drivers::invoke::HANDLE_PREFIX) {
                        return Err(
                            "a video's `inputs` are files of the working directory, by path; to start or end on an image of the conversation, give its handle as `params.first_frame` or `params.last_frame`".to_owned()
                        );
                    }
                    let kind = match Path::new(path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(str::to_ascii_lowercase)
                        .as_deref()
                    {
                        Some("mp4" | "mov" | "webm" | "mkv") => "video",
                        Some("wav") => "audio",
                        _ => "image",
                    };
                    Ok(json!({ "path": path, "kind": kind }))
                })
                .collect::<Result<Vec<_>, String>>()?;
            if !references.is_empty() {
                video.insert("references".into(), Value::Array(references));
            }
        }
        let done = generate(
            &self.endpoint,
            &self.config,
            &self.workdir,
            &Value::Object(video),
            &mut |_| {},
        )
        .await?;
        // In a conversation of the sven service the clip is also its file:
        // shown to the person and passed on by handle.
        let published = match (&self.config.session, &done.file_id) {
            (Some(_), Some(id)) if id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') => {
                Some(ArtifactRef {
                    uri: format!("{}{id}", sven_model_drivers::invoke::HANDLE_PREFIX),
                    name: done
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("video.mp4")
                        .to_owned(),
                    mime_type: "video/mp4".into(),
                    size: std::fs::metadata(&done.path)
                        .map(|m| m.len())
                        .unwrap_or_default(),
                })
            }
            _ => None,
        };
        let handle = published
            .as_ref()
            .map(|a| format!(" It is {} in the conversation.", a.uri))
            .unwrap_or_default()
            + &done
                .job_id
                .as_ref()
                .map(|id| format!(" It was made by job {id}: give `previous_job_id: {id}` to continue it, where the model sells that."))
                .unwrap_or_default();
        if done.reused {
            return Ok((
                format!("The video is at {} (made earlier for the same request; another seed makes another take).{handle}", done.shown),
                published,
            ));
        }
        let mut line = format!(
            "Saved the video to {} ({} bytes",
            done.shown,
            done.bytes.unwrap_or_default()
        );
        if let Some(seconds) = done.seconds {
            line.push_str(&format!(", {seconds:.1} s"));
        }
        line.push_str(").");
        line.push_str(&handle);
        Ok((line, published))
    }
}

/// The arguments `run_model` takes.
const RUN_ARGUMENTS: [&str; 8] = [
    "model",
    "operation",
    "prompt",
    "inputs",
    "params",
    "output",
    "name",
    "keep_as",
];

#[async_trait]
impl Tool for RunModelTool {
    fn name(&self) -> &str {
        "run_model"
    }
    fn summary(&self) -> &str {
        "Run one model once (chat, vision, image, speech, audio, video); files in and out by handle"
    }
    fn description(&self) -> &str {
        "Runs one model once and answers its text and the files it made. Give `model` (an id from the model list), `operation` (chat, image, speech, transcribe, audio or video, as the model's parameters say), `prompt` (what to make, ask or say), `inputs` (files it reads: handles like mcp-file://files/<id> from the conversation or an earlier run, or paths in the working directory; e.g. the image to edit, the picture to describe; for a chat model whose input includes them, a PDF, a sound or a video to read) and `params` (the model's own parameters by name, e.g. aspect_ratio, resolution, voice, format, duration; for a video that offers them, `first_frame` and `last_frame`, each an image handle or path, to start and end on, and `previous_job_id`, the job an earlier video's answer named, to continue that clip; for a chat, `response_format`, e.g. {\"type\": \"json_schema\", \"json_schema\": {\"name\": \"fields\", \"strict\": true, \"schema\": {...}}}, to have the answer as that JSON, which the service checks and says when it is not, and `pdf_engine: \"pdf-text\"` to give a PDF as its text to a model that reads no files; for speech, `turns` ([{text, voice, instructions}]) to voice several speakers, and a sound in `inputs` (with `reference_transcript`) to speak in that voice, for a model that clones one). What it makes is shown to the person and answered by handle; pass a handle in `inputs` to the next run to edit or reuse it, without reading it yourself. Give `name`, a short name for what it makes (e.g. `sven-slap-1`), so the person finds it among the conversation's files; `output` also saves the first file at that path in the working directory, and names it so when no `name` is given. Run once for each thing the person asked for: one image asked for is one run, of one model. Every run is charged and a video takes minutes, so before running several (to compare models, or variants nobody asked for) ask the person first, saying how many; when they asked for several, run them in their own calls. What a run was charged is shown to the person beside the conversation: do not work it out or report it."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "model": { "type": "string", "description": "A model id from the model list" },
                "operation": { "type": "string", "enum": ["chat", "image", "speech", "transcribe", "audio", "video"] },
                "prompt": { "type": "string", "description": "What to make, ask or say" },
                "inputs": { "type": "array", "items": { "type": "string" }, "description": "Files it reads: handles (mcp-file://files/...) or paths in the working directory" },
                "params": { "type": "object", "description": "The operation's parameters, as the model lists them" },
                "output": { "type": "string", "description": "Also save the first file at this path in the working directory" },
                "name": { "type": "string", "description": "A short name for what it makes, e.g. sven-slap-1; the file's extension is added" },
                "keep_as": { "type": "string", "enum": ["markdown", "json", "text"], "description": "chat only: keep the answer as a file and get its handle, not its text (a page's OCR, extracted fields)" }
            },
            "required": ["model", "operation", "prompt"],
            "additionalProperties": false
        })
    }
    fn default_policy(&self) -> ApprovalPolicy {
        // It spends money.
        ApprovalPolicy::Ask
    }
    fn kernel_capability(&self) -> ToolCapability {
        // The widest effect of a call: a new file in the working directory.
        ToolCapability::WriteFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let Some(args) = call.args.as_object() else {
            return ToolOutput::err(&call.id, "the arguments must be an object");
        };
        if let Some(unknown) = args.keys().find(|k| !RUN_ARGUMENTS.contains(&k.as_str())) {
            return ToolOutput::err(
                &call.id,
                format!("unknown argument `{unknown}`; parameters go in `params`"),
            );
        }
        let Some(operation) = args.get("operation").and_then(Value::as_str) else {
            return ToolOutput::err(&call.id, "`operation` is required: see model_schema");
        };
        if operation == "video" {
            return match self.video(args).await {
                Ok((text, published)) => {
                    ToolOutput::ok(&call.id, text).with_artifacts(published.into_iter().collect())
                }
                Err(error) => ToolOutput::err(&call.id, error),
            };
        }
        let output = args.get("output").and_then(Value::as_str);
        let body = match invocation::request(&self.workdir, args, operation) {
            Ok(body) => body,
            Err(error) => return ToolOutput::err(&call.id, error),
        };
        // A provider of its own (a bring-your-own key) is called directly;
        // the sven service runs the invocation as the person.
        let direct = self
            .config
            .provider
            .as_deref()
            .is_some_and(|p| p != sven_model_drivers::sven_service::PROVIDER);
        if direct {
            direct::run(
                &self.endpoint,
                &self.config,
                &self.workdir,
                &call.id,
                &body,
                output,
            )
            .await
        } else {
            let client = invoker(&self.endpoint, &self.config);
            invocation::run(
                &client,
                &self.config,
                &self.workdir,
                &call.id,
                &body,
                output,
            )
            .await
        }
    }
}
