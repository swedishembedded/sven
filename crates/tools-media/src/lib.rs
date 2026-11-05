// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that produce and edit
// video for its clients. If your team needs expertise in generative media
// workflows then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Generative media as two tools, discovered rather than hard-coded:
//!
//! - `models` lists the models the configured provider generates video with:
//!   what each renders (lengths, resolutions, sound), the edits it offers and
//!   its price per second.
//! - `run_model` runs one and writes the clip into the working directory, then
//!   answers its path. An edit (`extend`, `upscale`, ...) names the clip to
//!   work on by path and gets a new file back.
//! - `join_videos` puts clips one after another into one film ([`mp4`]),
//!   copying them as they are: the last step of a multi-shot film.
//!
//! Any OpenRouter-compatible video API serves: a local brain, the sven
//! service, OpenRouter itself (`sven_model_drivers::video`).
//!
//! **A request is named by what it asks.** The job's idempotency key is a
//! digest of the provider, the model and the canonical arguments, and the clip
//! is written under that name. Asking again for the same thing, in this
//! session or after a restart, answers the same job or the file already
//! written, and never pays for a second render; another take is another
//! `seed`. A job that failed is asked again under a fresh name.

pub mod mp4;

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use sven_hsm::ToolCapability;
use sven_model_drivers::video::{VideoClient, VideoModel, VideoState};
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};

/// The largest clip sent inline as an edit's input.
const MAX_INPUT_BYTES: u64 = 40 * 1024 * 1024;
/// How often a running job is asked about.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// Failed attempts at the same request before the tool gives up.
const MAX_ATTEMPTS: u32 = 3;

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
        }
    }
}

/// Where the tools generate: a provider's OpenAI-style base URL and key.
#[derive(Debug, Clone)]
pub struct MediaEndpoint {
    pub base_url: String,
    pub api_key: String,
}

fn client(endpoint: &MediaEndpoint) -> VideoClient {
    VideoClient::new(&endpoint.base_url, &endpoint.api_key)
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
    if !model.edits.is_empty() {
        parts.push(format!("edits: {}", model.edits.join(", ")));
    }
    if let Some((key, price)) = model.pricing.first() {
        parts.push(format!("${price}/s ({key})"));
    }
    parts.join(", ")
}

// ------------------------------------------------------------------ models

pub struct ModelsTool {
    endpoint: MediaEndpoint,
}

impl ModelsTool {
    #[must_use]
    pub fn new(endpoint: MediaEndpoint) -> Self {
        Self { endpoint }
    }
}

#[async_trait]
impl Tool for ModelsTool {
    fn name(&self) -> &str {
        "models"
    }
    fn summary(&self) -> &str {
        "List the models that generate video, what each renders and costs"
    }
    fn description(&self) -> &str {
        "Lists the generative video models this session can run: each one's id, what it makes (text->video), the lengths and resolutions it renders, whether it makes sound, the edits it can do to an existing clip (extend, upscale, ...) and its price per second. Use it to choose a model before generating."
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match client(&self.endpoint).models().await {
            Ok(models) if models.is_empty() => {
                ToolOutput::ok(&call.id, "No video model is available from this provider.")
            }
            Ok(models) => ToolOutput::ok(
                &call.id,
                models.iter().map(describe).collect::<Vec<_>>().join("\n"),
            ),
            Err(error) => {
                ToolOutput::err(&call.id, format!("Could not list the models: {error:#}"))
            }
        }
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

/// The request a call makes, without the input clip (named by its digest),
/// and where the clip is also to be saved.
#[derive(Debug, Clone, PartialEq)]
struct Planned {
    body: Map<String, Value>,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
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

/// Read the call's arguments into the request body; refuses anything the
/// schema does not name.
fn plan(args: &Value, workdir: &Path) -> Result<Planned, String> {
    let args = args.as_object().ok_or("the arguments must be an object")?;
    let known = [
        "model",
        "prompt",
        "duration",
        "resolution",
        "aspect_ratio",
        "generate_audio",
        "seed",
        "edit",
        "input_video",
        "params",
        "output",
    ];
    if let Some(unknown) = args.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(format!("unknown argument `{unknown}`"));
    }
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or("`model` is required: call models to choose one")?;
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or("`prompt` is required")?;
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("prompt".into(), json!(prompt));
    for key in [
        "duration",
        "resolution",
        "aspect_ratio",
        "generate_audio",
        "seed",
    ] {
        if let Some(value) = args.get(key) {
            body.insert(key.into(), value.clone());
        }
    }
    let edit = args.get("edit").and_then(Value::as_str);
    let input = match args.get("input_video").and_then(Value::as_str) {
        Some(path) => Some(file_to_read(workdir, path, "input_video")?),
        None => None,
    };
    let output = match args.get("output") {
        Some(Value::String(path)) => Some(file_to_write(workdir, path, "output")?),
        Some(_) => return Err("`output` must be a path".into()),
        None => None,
    };
    if edit.is_some() != input.is_some() {
        return Err(
            "an edit names both `edit` (from models) and the clip to edit as `input_video`".into(),
        );
    }
    if let Some(edit) = edit {
        let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Err("`params` must be an object".into());
        }
        body.insert(
            "provider".into(),
            json!({ "options": { "brain": { "action": edit, "params": params } } }),
        );
    } else if args.contains_key("params") {
        return Err("`params` belong to an edit".into());
    }
    Ok(Planned {
        body,
        input,
        output,
    })
}

/// The name of a request: the provider, the model and every argument, the
/// input clip by its content, and the attempt.
fn request_key(
    base_url: &str,
    planned: &Planned,
    input_digest: Option<&str>,
    attempt: u32,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(base_url.as_bytes());
    hasher.update([0]);
    // serde_json's map is ordered by key, so equal requests serialize equally.
    hasher.update(serde_json::to_vec(&planned.body).unwrap_or_default());
    hasher.update([0]);
    hasher.update(input_digest.unwrap_or("").as_bytes());
    if attempt > 0 {
        hasher.update(format!("\0retry {attempt}").as_bytes());
    }
    format!("sven-{}", &hex::encode(hasher.finalize())[..32])
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

/// The clip an edit sends: the provider's own file when it has one, else the
/// bytes inline.
async fn input_reference(path: &Path) -> Result<(Value, String), String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(format!(
            "{} is larger than {MAX_INPUT_BYTES} bytes",
            path.display()
        ));
    }
    let digest = hex::encode(Sha256::digest(&bytes));
    let stored = tokio::fs::read(sidecar_path(path))
        .await
        .ok()
        .and_then(|raw| serde_json::from_slice::<Sidecar>(&raw).ok())
        .and_then(|s| s.file_id);
    let url = match stored {
        Some(id) => format!("file:{id}"),
        None => {
            let media = if path.extension().and_then(|e| e.to_str()) == Some("webm") {
                "video/webm"
            } else {
                "video/mp4"
            };
            format!(
                "data:{media};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            )
        }
    };
    Ok((
        json!({ "type": "video_url", "video_url": { "url": url } }),
        digest,
    ))
}

impl RunModelTool {
    async fn run(&self, args: &Value) -> Result<String, String> {
        let mut planned = plan(args, &self.workdir)?;
        let input = match &planned.input {
            Some(path) => Some(input_reference(path).await?),
            None => None,
        };
        let digest = input.as_ref().map(|(_, digest)| digest.clone());
        if let Some((reference, _)) = input {
            planned.body.insert("input_video".into(), reference);
        }
        let video = client(&self.endpoint);
        let dir = self.workdir.join(&self.config.dir);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(self.config.timeout_secs);
        for attempt in 0..MAX_ATTEMPTS {
            let key = request_key(
                &self.endpoint.base_url,
                &planned,
                digest.as_deref(),
                attempt,
            );
            let clip = dir.join(format!("{key}.mp4"));
            if clip.exists() {
                let shown = self.save_as(&clip, planned.output.as_deref()).await?;
                return Ok(format!("The video is at {shown} (made earlier for the same request; another seed makes another take)."));
            }
            let mut status = video
                .create(&Value::Object(planned.body.clone()), &key)
                .await
                .map_err(|e| format!("{e:#}"))?;
            while !status.state.finished() {
                if tokio::time::Instant::now() > deadline {
                    let _ = video.cancel(&status.id).await;
                    return Err(format!(
                        "the video was not ready within {} s; it was asked to stop",
                        self.config.timeout_secs
                    ));
                }
                tokio::time::sleep(POLL_EVERY).await;
                status = video
                    .status(&status.id)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
            }
            match status.state {
                VideoState::Completed => {
                    let bytes = video
                        .download(&status.id, &clip)
                        .await
                        .map_err(|e| format!("{e:#}"))?;
                    let model = planned.body["model"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let sidecar = Sidecar {
                        model,
                        file_id: status.file_id.clone(),
                        seconds: status.seconds,
                    };
                    let _ = tokio::fs::write(
                        sidecar_path(&clip),
                        serde_json::to_vec_pretty(&sidecar).unwrap_or_default(),
                    )
                    .await;
                    let shown = self.save_as(&clip, planned.output.as_deref()).await?;
                    let mut line = format!("Saved the video to {shown} ({bytes} bytes");
                    if let Some(seconds) = status.seconds {
                        line.push_str(&format!(", {seconds:.1} s"));
                    }
                    if let Some(cost) = status.cost {
                        line.push_str(&format!(", ${cost:.4}"));
                    }
                    line.push_str(").");
                    return Ok(line);
                }
                // A failed request is asked again under a fresh name: the
                // same name would only answer the same failure.
                VideoState::Failed(_) if attempt + 1 < MAX_ATTEMPTS => {}
                VideoState::Failed(reason) => {
                    return Err(format!("the video could not be made: {reason}"))
                }
                VideoState::Cancelled => {
                    return Err("the video was stopped before it was ready".into())
                }
                VideoState::Expired => {
                    return Err("the video expired before it was downloaded; ask again".into())
                }
                VideoState::Pending | VideoState::InProgress => {
                    unreachable!("the loop above waits for a finished state")
                }
            }
        }
        Err("the video could not be made".into())
    }

    /// The clip also written at `output`, when asked (copied whole, then
    /// renamed into place), and the path to show for it.
    async fn save_as(&self, clip: &Path, output: Option<&Path>) -> Result<String, String> {
        let Some(output) = output else {
            return Ok(shown(&self.workdir, clip));
        };
        let partial = output.with_extension("mp4.part");
        tokio::fs::copy(clip, &partial)
            .await
            .and(tokio::fs::rename(&partial, output).await)
            .map_err(|e| format!("saving the video as {}: {e}", output.display()))?;
        Ok(shown(&self.workdir, output))
    }
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

#[async_trait]
impl Tool for RunModelTool {
    fn name(&self) -> &str {
        "run_model"
    }
    fn summary(&self) -> &str {
        "Generate or edit a video with a generative model; writes the clip to a file"
    }
    fn description(&self) -> &str {
        "Runs a generative video model and writes the result into the working directory, answering its path. Give `model` and `prompt`, and optionally `duration` (seconds), `resolution` (480p, 720p, 1080p, ...), `aspect_ratio` (16:9, 9:16, 1:1, ...), `generate_audio` and `seed`. To edit a clip, name the edit (`edit`, one the model offers, e.g. extend) and the clip (`input_video`, a path in the working directory), with the edit's own `params`. `output` also saves the clip at that path. The same request answers the same clip without paying again; change `seed` for another take. A video takes from seconds to many minutes and is charged for the seconds delivered."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "model": { "type": "string", "description": "The id of a video model" },
                "prompt": { "type": "string", "description": "What the video shows" },
                "duration": { "type": "number", "description": "Seconds" },
                "resolution": { "type": "string" },
                "aspect_ratio": { "type": "string" },
                "generate_audio": { "type": "boolean" },
                "seed": { "type": "integer" },
                "edit": { "type": "string", "description": "An edit the model offers, with input_video" },
                "input_video": { "type": "string", "description": "Path of the clip to edit, in the working directory" },
                "params": { "type": "object", "description": "The edit's own parameters" },
                "output": { "type": "string", "description": "Also save the clip at this path in the working directory" }
            },
            "required": ["model", "prompt"],
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
        match self.run(&call.args).await {
            Ok(text) => ToolOutput::ok(&call.id, text),
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
