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

mod direct;
mod generate;
mod handover;
mod invocation;
pub mod mp4;
mod request;
mod run_model;

#[cfg(any(test, feature = "fake-server"))]
pub mod fake;

pub use generate::{generate, Generated};
pub use handover::{fetch_to, hand_over, Handover};
pub use run_model::RunModelTool;

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
    /// Whether the session is sven's own (its tool set detected, not an
    /// embedding host's preset), so the media tools may spend this machine's
    /// sign-in. Set by the session builder, never read from a file.
    #[serde(skip)]
    pub offered: bool,
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
            offered: false,
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

/// The kinds of thing a model makes, as the model list names them, in the
/// order an overview gives them, with what each is in a person's words.
const KINDS: [(&str, &str); 7] = [
    ("text", "chat, and vision: reading images"),
    ("image", "pictures"),
    ("speech", "text to speech, voices"),
    ("audio", "music and sound"),
    ("video", "video clips"),
    ("transcription", "speech to text"),
    ("embeddings", "vectors for search"),
];

/// The kind `word` names: the list's own name, or the everyday word for it.
fn kind_of(word: &str) -> String {
    match word.trim() {
        "music" | "song" | "songs" | "sound" | "sounds" | "sfx" | "jingle" => "audio",
        "voice" | "voices" | "tts" | "speak" | "narration" | "say" => "speech",
        "picture" | "pictures" | "photo" | "drawing" | "illustration" | "images" => "image",
        "stt" | "asr" | "transcribe" | "transcript" => "transcription",
        "chat" | "vision" | "llm" => "text",
        "clip" | "film" | "movie" | "videos" => "video",
        "embedding" | "embed" => "embeddings",
        other => other,
    }
    .to_owned()
}

fn makes(model: &ListedModel, kind: &str) -> bool {
    model.output.iter().any(|o| o == kind)
}

/// How many models make each kind, with a few of each, and what nothing makes.
fn overview(models: &[ListedModel]) -> String {
    let mut lines = vec![format!(
        "This session can run {} models, by what they make:",
        models.len()
    )];
    let mut missing = Vec::new();
    for (kind, means) in KINDS {
        let of: Vec<&str> = models
            .iter()
            .filter(|m| makes(m, kind))
            .map(|m| m.id.as_str())
            .collect();
        if of.is_empty() {
            missing.push(format!("{kind} ({means})"));
            continue;
        }
        let shown: Vec<&str> = of.iter().take(3).copied().collect();
        lines.push(format!(
            "- {kind} ({means}): {}, e.g. {}",
            of.len(),
            shown.join(", ")
        ));
    }
    if !missing.is_empty() {
        lines.push(format!("Nothing here makes {}.", missing.join(", ")));
    }
    lines.push(r#"List one kind with `output`, e.g. {"output": "image"}; add `query` for words in a model's name."#.into());
    lines.join("\n")
}

/// What to say when nothing matches: that nothing makes `kind` at all, and
/// to ask the person rather than look further; or, when some model does,
/// those models.
fn no_match(models: &[ListedModel], kind: Option<&str>) -> String {
    let available: Vec<String> = KINDS
        .iter()
        .filter_map(|(k, _)| {
            let n = models.iter().filter(|m| makes(m, k)).count();
            (n > 0).then(|| format!("{k} ({n})"))
        })
        .collect();
    match kind {
        Some(kind) if !models.iter().any(|m| makes(m, kind)) => {
            let means = KINDS.iter().find(|(k, _)| *k == kind).map(|(_, m)| format!(" ({m})")).unwrap_or_default();
            format!(
                "No model this session can run makes {kind}{means}. It can run: {}. This list is every model there is, so another name will not find one: tell the person, say what could be done instead, and ask them how to go on.",
                available.join(", ")
            )
        }
        Some(kind) => {
            let of: Vec<String> = models.iter().filter(|m| makes(m, kind)).take(MAX_LISTED).map(line).collect();
            format!("No {kind} model matches that; these are every {kind} model:\n{}", of.join("\n"))
        }
        None => format!(
            "No model matches. It can run: {}. List one kind with `output` instead of guessing names, and ask the person if what they want is not there.",
            available.join(", ")
        ),
    }
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
        "List the models this session can run, by what they make (text, image, speech, audio, video, transcription)"
    }
    fn description(&self) -> &str {
        "Lists the models this session can run. Call it once with no arguments first: it answers how many models make each kind (text, image, speech, audio for music and sound, video, transcription, embeddings), a few of each, and what nothing makes. Then list one kind with `output` (e.g. image, speech, audio; everyday words such as music or voice work too), narrowed with `input` (what it must read, e.g. image) or `query` (words in its id or name). Each line gives its id, what it reads and makes, the operation it is run as, its voices and list prices, which are for choosing between models: what a run is charged is often less and is shown to the person beside the conversation, so never add up or report costs from them. The list is everything there is: when nothing makes what the person asked for, tell them and ask, instead of searching under other names. Then ask for the chosen model's parameters."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "output": { "type": "string", "description": "What the model makes: text, image, speech (text to speech), audio (music, sound), video, transcription, embeddings" },
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
        let (output, input, query) = (
            arg("output").map(|o| kind_of(&o)),
            arg("input"),
            arg("query"),
        );
        let models = match every_model(&self.endpoint).await {
            Ok(models) => models,
            Err(error) => return ToolOutput::err(&call.id, error),
        };
        if output.is_none() && input.is_none() && query.is_none() {
            return ToolOutput::ok(&call.id, overview(&models));
        }
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
            return ToolOutput::ok(&call.id, no_match(&models, output.as_deref()));
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
