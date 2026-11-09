// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that produce and edit
// video for its clients. If your team needs expertise in generative media
// workflows then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Every model a provider serves, as three tools, discovered rather than
//! hard-coded:
//!
//! - `models` lists them, one line each, filtered by what they read and make
//!   (`input`, `output`) or by text: chat and vision, image, speech, music,
//!   transcription and video models alike.
//! - `model_schema` says everything needed to run one: its operations, the
//!   parameters each takes (enums and ranges), its voices and prices.
//! - `run_model` runs one once. Inputs and outputs are files by handle
//!   ([`invocation`]): in a conversation of the sven service a model's output
//!   stays there and is shown to the person, and the next model reads it by
//!   the same handle; elsewhere outputs are fetched into the working
//!   directory. Video is a durable job ([`generate`]) written to a file.
//! - `join_videos` puts clips one after another into one film ([`mp4`]),
//!   copying them as they are: the last step of a multi-shot film.
//!
//! Any OpenRouter-compatible API serves video: a local brain, the sven
//! service, OpenRouter itself (`sven_model_drivers::video`); every other
//! operation needs the invocation API of the sven service
//! (`sven_model_drivers::invoke`).
//!
//! **A clip is named by what it asks.** A video job's idempotency key is a
//! digest of the provider, the model and the canonical arguments, and the clip
//! is written under that name. Asking again for the same thing, in this
//! session or after a restart, answers the same job or the file already
//! written, and never pays for a second render; another take is another
//! `seed`. A job that failed is asked again under a fresh name. Every other
//! invocation is named by its call, so asking again is a new picture.

mod generate;
mod invocation;
pub mod mp4;
mod request;

#[cfg(any(test, feature = "fake-server"))]
pub mod fake;

pub use generate::{generate, Generated};

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use sven_hsm::ToolCapability;
use sven_model_drivers::invoke::{InvocationClient, ListedModel};
use sven_model_drivers::video::{VideoClient, VideoModel};
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};

/// The largest clip sent inline as an edit's input.
const MAX_INPUT_BYTES: u64 = 40 * 1024 * 1024;

/// The `tools.media` section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MediaConfig {
    /// The `providers.<name>` entry to generate with (base URL and key); none
    /// means the sven service, signed in or configured.
    pub provider: Option<String>,
    /// Where clips are written, relative to the working directory.
    pub dir: PathBuf,
    /// The longest one generation may take, in seconds.
    pub timeout_secs: u64,
    /// The conversation of the sven service outputs belong to: set by
    /// `sven rc` for each session it hosts, never read from a file.
    #[serde(skip)]
    pub session: Option<String>,
    /// The sven service this process is signed in to, with its key: set at
    /// run time by `sven rc` from its sign-in, never read from a file. Used
    /// unless `provider` names another.
    #[serde(skip)]
    pub service: Option<MediaEndpoint>,
}

impl MediaConfig {
    /// The keys of the `tools.media` section.
    #[must_use]
    pub fn schema() -> sven_config::Schema {
        sven_config::Schema::keys(&["provider", "dir", "timeout_secs"])
    }
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            provider: None,
            dir: PathBuf::from(".sven/media"),
            timeout_secs: 30 * 60,
            session: None,
            service: None,
        }
    }
}

/// Where the tools generate: a provider's OpenAI-style base URL and key.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaEndpoint {
    pub base_url: String,
    pub api_key: String,
}

fn client(endpoint: &MediaEndpoint) -> VideoClient {
    VideoClient::new(&endpoint.base_url, &endpoint.api_key)
}

fn invoker(endpoint: &MediaEndpoint, config: &MediaConfig) -> InvocationClient {
    InvocationClient::new(
        &endpoint.base_url,
        &endpoint.api_key,
        config.session.clone(),
    )
}

/// One line per model, what a model needs to choose one.
fn describe(model: &VideoModel) -> String {
    let mut parts = vec![format!("{}: text->video", model.id)];
    if !model.durations.is_empty() {
        let (lo, hi) = (
            model.durations.iter().min().copied().unwrap_or(0),
            model.durations.iter().max().copied().unwrap_or(0),
        );
        parts.push(if lo == hi {
            format!("{lo} s")
        } else {
            format!("{lo}-{hi} s")
        });
    }
    if !model.resolutions.is_empty() {
        parts.push(model.resolutions.join("/"));
    }
    if model.generate_audio == Some(true) {
        parts.push("sound".into());
    }
    if !model.supported_reference_types.is_empty() {
        parts.push(format!(
            "references: {}",
            model.supported_reference_types.join("/")
        ));
    }
    if !model.actions.is_empty() {
        parts.push(format!("actions: {}", model.actions.join(", ")));
    }
    if !model.edits.is_empty() {
        parts.push(format!("edits: {}", model.edits.join(", ")));
    }
    if let Some((key, price)) = model.pricing.first() {
        parts.push(format!("${price}/s ({key})"));
    }
    parts.join(", ")
}

// ------------------------------------------------------------------ models

/// Every model the provider serves, video models included, as one list.
async fn every_model(endpoint: &MediaEndpoint) -> Result<Vec<ListedModel>, String> {
    let listed = invoker(endpoint, &MediaConfig::default()).models().await;
    let videos = client(endpoint).models().await;
    let mut models = match (listed, &videos) {
        (Ok(models), _) => models,
        (Err(_), Ok(_)) => Vec::new(),
        (Err(error), Err(_)) => return Err(format!("Could not list the models: {error:#}")),
    };
    for video in videos.unwrap_or_default() {
        if let Some(known) = models.iter_mut().find(|m| m.id == video.id) {
            if !known.output.iter().any(|o| o == "video") {
                known.output.push("video".into());
            }
            continue;
        }
        models.push(ListedModel {
            id: video.id.clone(),
            name: video.name.clone(),
            description: Some(describe(&video)),
            input: std::iter::once("text".to_owned())
                .chain(video.supported_reference_types.iter().cloned())
                .collect(),
            output: vec!["video".into()],
            pricing: video.pricing.clone(),
            voices: Vec::new(),
            parameters: Value::Null,
        });
    }
    Ok(models)
}

/// One line per model: what it reads and makes, how it is run, its price.
fn line(model: &ListedModel) -> String {
    let mut line = format!(
        "{}: {} -> {}",
        model.id,
        model.input.join("+"),
        model.output.join("+")
    );
    let ops = model.operations();
    if !ops.is_empty() {
        line.push_str(&format!("; run as {}", ops.join("/")));
    }
    if !model.voices.is_empty() {
        line.push_str(&format!("; {} voices", model.voices.len()));
    }
    let prices: Vec<String> = model
        .pricing
        .iter()
        .filter(|(_, v)| v.parse::<f64>().is_ok_and(|p| p > 0.0))
        .take(3)
        .map(|(k, v)| format!("{k} ${v}"))
        .collect();
    if !prices.is_empty() {
        line.push_str(&format!("; {}", prices.join(", ")));
    }
    line
}

pub struct ModelsTool {
    endpoint: MediaEndpoint,
}

impl ModelsTool {
    #[must_use]
    pub fn new(endpoint: MediaEndpoint) -> Self {
        Self { endpoint }
    }
}

/// The most lines `models` answers at once.
const MAX_LISTED: usize = 60;

#[async_trait]
impl Tool for ModelsTool {
    fn name(&self) -> &str {
        "models"
    }
    fn summary(&self) -> &str {
        "List the models this session can run (chat, vision, image, speech, music, transcription, video)"
    }
    fn description(&self) -> &str {
        "Lists the models this session can run: one line each with its id, what it reads and makes (text, image, audio, speech, video), the operation it is run as (chat, image, speech, transcribe, audio, video), its voices and prices. Filter with `output` (what it makes, e.g. image or speech), `input` (what it must read, e.g. image) and `query` (words in its id or name, e.g. `nano banana`). Then ask for the chosen model's parameters."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "output": { "type": "string", "description": "What the model makes: text, image, speech, audio, video, transcription" },
                "input": { "type": "string", "description": "What the model must read: text, image, audio, file, video" },
                "query": { "type": "string", "description": "Words that must appear in its id or name" }
            },
            "additionalProperties": false
        })
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let arg = |name: &str| {
            call.args
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
        };
        let (output, input, query) = (arg("output"), arg("input"), arg("query"));
        let models = match every_model(&self.endpoint).await {
            Ok(models) => models,
            Err(error) => return ToolOutput::err(&call.id, error),
        };
        let matching: Vec<&ListedModel> = models
            .iter()
            .filter(|m| {
                output
                    .as_ref()
                    .is_none_or(|o| m.output.iter().any(|x| x == o))
            })
            .filter(|m| {
                input
                    .as_ref()
                    .is_none_or(|i| m.input.iter().any(|x| x == i))
            })
            .filter(|m| {
                query.as_ref().is_none_or(|q| {
                    let hay = format!("{} {}", m.id, m.name.as_deref().unwrap_or(""))
                        .to_ascii_lowercase();
                    q.split_whitespace().all(|word| hay.contains(word))
                })
            })
            .collect();
        if matching.is_empty() {
            return ToolOutput::ok(&call.id, "No model matches; try fewer filters.");
        }
        let mut lines: Vec<String> = matching.iter().take(MAX_LISTED).map(|m| line(m)).collect();
        if matching.len() > MAX_LISTED {
            lines.push(format!(
                "... and {} more; narrow with `query`.",
                matching.len() - MAX_LISTED
            ));
        }
        ToolOutput::ok(&call.id, lines.join("\n"))
    }
}

// ------------------------------------------------------------------ model_schema

pub struct ModelSchemaTool {
    endpoint: MediaEndpoint,
}

impl ModelSchemaTool {
    #[must_use]
    pub fn new(endpoint: MediaEndpoint) -> Self {
        Self { endpoint }
    }
}

#[async_trait]
impl Tool for ModelSchemaTool {
    fn name(&self) -> &str {
        "model_schema"
    }
    fn summary(&self) -> &str {
        "Everything needed to run one model: operations, parameters, voices, prices"
    }
    fn description(&self) -> &str {
        "Answers everything needed to run `model`: the operations it is run as, the parameters each takes with their allowed values (enums, ranges), how many reference images it takes, its voices and its prices. Ask for it once you have chosen a model, before running it."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "model": { "type": "string", "description": "A model id from the model list" } },
            "required": ["model"],
            "additionalProperties": false
        })
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let Some(id) = call.args.get("model").and_then(Value::as_str) else {
            return ToolOutput::err(&call.id, "`model` is required");
        };
        let models = match every_model(&self.endpoint).await {
            Ok(models) => models,
            Err(error) => return ToolOutput::err(&call.id, error),
        };
        let Some(model) = models.iter().find(|m| m.id == id) else {
            return ToolOutput::err(
                &call.id,
                format!("No model `{id}`; list them with `models`."),
            );
        };
        let voices: Vec<&String> = model.voices.iter().take(100).collect();
        let schema = json!({
            "model": model.id,
            "name": model.name,
            "description": model.description.as_deref().map(|d| d.chars().take(400).collect::<String>()),
            "reads": model.input,
            "makes": model.output,
            "operations": model.operations(),
            "parameters": model.parameters,
            "voices": voices,
            "more_voices": model.voices.len().saturating_sub(voices.len()),
            "pricing": model.pricing.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect::<serde_json::Map<_, _>>(),
            "run_model": "Give `operation` (one of operations), `prompt`, `inputs` (handles or paths of the files it reads) and `params` (the parameters above, by name).",
        });
        ToolOutput::ok(
            &call.id,
            serde_json::to_string_pretty(&schema).unwrap_or_default(),
        )
    }
}

// ------------------------------------------------------------------ run_model

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

/// `path` as a file of `workdir` to write: relative, never above it, and its
/// directory created; refused when that directory leads outside.
fn file_to_write(workdir: &Path, path: &str, what: &str) -> Result<PathBuf, String> {
    let relative = Path::new(path);
    let inside = !path.is_empty()
        && relative.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
    if !inside || relative.file_name().is_none() {
        return Err(format!(
            "`{what}` must be a file path inside the working directory"
        ));
    }
    let target = workdir.join(relative);
    let parent = target.parent().unwrap_or(workdir);
    std::fs::create_dir_all(parent).map_err(|e| format!("`{what}` {}: {e}", parent.display()))?;
    let root = workdir
        .canonicalize()
        .map_err(|e| format!("the working directory: {e}"))?;
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("`{what}` {}: {e}", parent.display()))?;
    if !parent.starts_with(&root) {
        return Err(format!(
            "`{what}` must be a file path inside the working directory"
        ));
    }
    Ok(parent.join(relative.file_name().unwrap_or_default()))
}

/// `path` as an existing file of `workdir` to read.
fn file_to_read(workdir: &Path, path: &str, what: &str) -> Result<PathBuf, String> {
    let path = workdir.join(path);
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("`{what}` {}: {e}", path.display()))?;
    let root = workdir
        .canonicalize()
        .map_err(|e| format!("the working directory: {e}"))?;
    if !canonical.starts_with(&root) {
        return Err(format!("`{what}` must be a file in the working directory"));
    }
    Ok(canonical)
}

/// What the tool keeps beside a clip: the provider's file of it, when the
/// provider stores one, so an edit can name it there instead of uploading.
#[derive(Debug, Serialize, Deserialize)]
struct Sidecar {
    model: String,
    file_id: Option<String>,
    seconds: Option<f64>,
}

fn sidecar_path(clip: &Path) -> PathBuf {
    clip.with_extension("json")
}

impl RunModelTool {
    /// A video: a durable job written to a file ([`generate`]), its
    /// parameters as the video API names them.
    async fn video(&self, args: &serde_json::Map<String, Value>) -> Result<String, String> {
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
                            "a video reads files of the working directory, by path".to_owned()
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
        if done.reused {
            return Ok(format!("The video is at {} (made earlier for the same request; another seed makes another take).", done.shown));
        }
        let mut line = format!(
            "Saved the video to {} ({} bytes",
            done.shown,
            done.bytes.unwrap_or_default()
        );
        if let Some(seconds) = done.seconds {
            line.push_str(&format!(", {seconds:.1} s"));
        }
        if let Some(cost) = done.cost {
            line.push_str(&format!(", ${cost:.4}"));
        }
        line.push_str(").");
        Ok(line)
    }
}

/// The arguments `run_model` takes.
const RUN_ARGUMENTS: [&str; 6] = ["model", "operation", "prompt", "inputs", "params", "output"];

/// A path as the model and the person read it: relative to the working
/// directory.
fn shown(workdir: &Path, path: &Path) -> String {
    let root = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    path.strip_prefix(&root)
        .or_else(|_| path.strip_prefix(workdir))
        .unwrap_or(path)
        .display()
        .to_string()
}

#[async_trait]
impl Tool for RunModelTool {
    fn name(&self) -> &str {
        "run_model"
    }
    fn summary(&self) -> &str {
        "Run any model once (chat, vision, image, speech, music, video); files in and out by handle"
    }
    fn description(&self) -> &str {
        "Runs one model once and answers its text and the files it made. Give `model` (an id from the model list), `operation` (chat, image, speech, transcribe, audio or video, as the model's parameters say), `prompt` (what to make, ask or say), `inputs` (files it reads: handles like mcp-file://files/<id> from the conversation or an earlier run, or paths in the working directory; e.g. the image to edit, the picture to describe) and `params` (the model's own parameters by name, e.g. aspect_ratio, resolution, voice, format, duration). What it makes is shown to the person and answered by handle; pass a handle in `inputs` to the next run to edit or reuse it, without reading it yourself. `output` also saves the first file at that path in the working directory. To compare several, run each in its own call, at once. Every run is charged; a video takes minutes."
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
                "output": { "type": "string", "description": "Also save the first file at this path in the working directory" }
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
                Ok(text) => ToolOutput::ok(&call.id, text),
                Err(error) => ToolOutput::err(&call.id, error),
            };
        }
        let output = args.get("output").and_then(Value::as_str);
        match invocation::request(&self.workdir, args, operation) {
            Ok(body) => {
                invocation::run(
                    &invoker(&self.endpoint, &self.config),
                    &self.config,
                    &self.workdir,
                    &call.id,
                    &body,
                    output,
                )
                .await
            }
            Err(error) => ToolOutput::err(&call.id, error),
        }
    }
}

// ------------------------------------------------------------------ join_videos

pub struct JoinVideosTool {
    workdir: PathBuf,
}

impl JoinVideosTool {
    #[must_use]
    pub fn new(workdir: PathBuf) -> Self {
        Self { workdir }
    }

    async fn run(&self, args: &Value) -> Result<String, String> {
        let args = args.as_object().ok_or("the arguments must be an object")?;
        if let Some(unknown) = args
            .keys()
            .find(|k| !["clips", "output", "video_only"].contains(&k.as_str()))
        {
            return Err(format!("unknown argument `{unknown}`"));
        }
        let clips = args
            .get("clips")
            .and_then(Value::as_array)
            .filter(|c| !c.is_empty())
            .ok_or("`clips` is required: the clips to join, in order")?
            .iter()
            .map(|c| {
                c.as_str()
                    .ok_or_else(|| "`clips` holds paths".to_owned())
                    .and_then(|c| file_to_read(&self.workdir, c, "clips"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output = args
            .get("output")
            .and_then(Value::as_str)
            .ok_or("`output` is required")?;
        let output = file_to_write(&self.workdir, output, "output")?;
        let video_only = args
            .get("video_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let target = output.clone();
        let probe = tokio::task::spawn_blocking(move || mp4::join(&clips, &target, video_only))
            .await
            .map_err(|e| format!("joining stopped: {e}"))?
            .map_err(|e| format!("{e:#}"))?;
        Ok(format!(
            "Joined into {}: {:.2} s, {} frames, {}x{} {}{}.",
            shown(&self.workdir, &output),
            probe.duration_seconds,
            probe.video.frames,
            probe.video.width,
            probe.video.height,
            probe.video.codec,
            if probe.audio.is_some() {
                ", with sound"
            } else {
                ""
            },
        ))
    }
}

#[async_trait]
impl Tool for JoinVideosTool {
    fn name(&self) -> &str {
        "join_videos"
    }
    fn summary(&self) -> &str {
        "Join video clips one after another into one file"
    }
    fn description(&self) -> &str {
        "Joins MP4 clips (`clips`, paths in the working directory, in the order they play) into one file (`output`), copying them as they are: nothing is re-encoded, so the clips must come from the same model at the same size. Sound stays in sync with each clip's picture; clips with and without sound join only as pictures (`video_only`). Answers the film's length, frames and size."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "clips": { "type": "array", "items": { "type": "string" }, "description": "The clips, in order" },
                "output": { "type": "string", "description": "The file to write" },
                "video_only": { "type": "boolean", "description": "Leave the sound out" }
            },
            "required": ["clips", "output"],
            "additionalProperties": false
        })
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::WriteFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match self.run(&call.args).await {
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(error) => ToolOutput::err(&call.id, error),
        }
    }
}

#[cfg(test)]
mod tests;
