// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for reproducible, reference-driven
// video generation for its clients. If your team needs expertise in
// generative media pipelines then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The request a `run_model` call makes: its arguments checked at the
//! boundary, the files it names read once, and the name the request is given.
//!
//! A reference is an image, a clip or a sound of the working directory that
//! the prompt points at by order within its kind (`<Picture 1>`, `<Video 1>`,
//! `<Audio 1>`), so the order the call lists them in is part of the request.
//!
//! A frame (`first_frame`, `last_frame`) is an image the clip starts or ends
//! on: a file of the working directory, sent inline, or a handle of the
//! conversation (`mcp-file://files/<id>`), whose bytes stay with the service
//! that resolves it, for its own files only.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::{file_to_read, file_to_write, sidecar_path, Sidecar, MAX_INPUT_BYTES};

/// What a reference is, which decides how the provider reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Image,
    Video,
    Audio,
}

impl Kind {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    /// The media type of a file of this kind with extension `ext`, or none
    /// when a file of that name is not this kind.
    fn media_type(self, ext: &str) -> Option<&'static str> {
        match (self, ext) {
            (Self::Image, "png") => Some("image/png"),
            (Self::Image, "jpg" | "jpeg") => Some("image/jpeg"),
            (Self::Image, "webp") => Some("image/webp"),
            (Self::Video, "mp4") => Some("video/mp4"),
            (Self::Video, "mov") => Some("video/quicktime"),
            (Self::Video, "webm") => Some("video/webm"),
            (Self::Video, "mkv") => Some("video/x-matroska"),
            (Self::Audio, "wav") => Some("audio/wav"),
            _ => None,
        }
    }

    /// The reference as the provider reads it: `{"type": "image_url", "image_url": {"url": ..}}`.
    fn part(self, url: String) -> Value {
        let tag = format!("{}_url", self.name());
        json!({ "type": tag, tag.clone(): { "url": url } })
    }
}

/// An image a clip starts or ends on.
#[derive(Debug, Clone, PartialEq)]
enum FrameSource {
    /// A file of the working directory, sent inline.
    Path {
        path: PathBuf,
        media_type: &'static str,
    },
    /// A handle of the conversation, which the service resolves.
    Handle(String),
}

#[derive(Debug, Clone, PartialEq)]
struct Frame {
    /// `first_frame` or `last_frame`, as the video API names it.
    kind: &'static str,
    source: FrameSource,
}

/// The frames a model may start or end on, as the video API names them.
pub(crate) const FRAME_KINDS: [&str; 2] = ["first_frame", "last_frame"];

/// A handle of the conversation: its prefix and a file id, nothing else.
fn is_handle(text: &str) -> bool {
    text.strip_prefix(sven_model_drivers::invoke::HANDLE_PREFIX)
        .is_some_and(|id| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
}

fn frame(value: &Value, kind: &'static str, workdir: &Path) -> Result<Frame, String> {
    let named = value.as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
        format!("`{kind}` is an image: a handle (mcp-file://files/<id>) or a path")
    })?;
    if named.starts_with(sven_model_drivers::invoke::HANDLE_PREFIX) {
        if !is_handle(named) {
            return Err(format!(
                "`{kind}` is not a handle of the conversation: {named}"
            ));
        }
        return Ok(Frame {
            kind,
            source: FrameSource::Handle(named.to_owned()),
        });
    }
    let path = file_to_read(workdir, named, kind)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let media_type = Kind::Image
        .media_type(&ext)
        .ok_or_else(|| format!("`{kind}` {named} is not an image (png, jpg or webp)"))?;
    Ok(Frame {
        kind,
        source: FrameSource::Path { path, media_type },
    })
}

#[derive(Debug, Clone, PartialEq)]
struct Reference {
    kind: Kind,
    source: ReferenceSource,
}

/// Where a reference's file is.
#[derive(Debug, Clone, PartialEq)]
enum ReferenceSource {
    /// A file of the working directory, sent by reference when the service
    /// holds it unchanged (`held`), else inline.
    Path {
        path: PathBuf,
        media_type: &'static str,
    },
    /// A handle of the conversation, which the service resolves.
    Handle(String),
}

/// The request a call makes, without the files it names (they are read when
/// the request is sent and named by their digests), and where the clip is
/// also to be saved.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Planned {
    pub(crate) body: Map<String, Value>,
    input: Option<PathBuf>,
    references: Vec<Reference>,
    frames: Vec<Frame>,
    pub(crate) output: Option<PathBuf>,
}

/// `size` as `WIDTHxHEIGHT` of two positive integers, in its canonical spelling.
fn size(value: &Value) -> Result<String, String> {
    let wrong = || "`size` must be WIDTHxHEIGHT, two positive integers, e.g. 832x480".to_owned();
    let (width, height) = value
        .as_str()
        .and_then(|s| s.split_once('x'))
        .ok_or_else(wrong)?;
    let dimension = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse::<u32>().ok().filter(|n| *n > 0))
            .flatten()
            .ok_or_else(wrong)
    };
    Ok(format!("{}x{}", dimension(width)?, dimension(height)?))
}

fn references(value: &Value, workdir: &Path) -> Result<Vec<Reference>, String> {
    let list = value
        .as_array()
        .filter(|l| !l.is_empty())
        .ok_or("`references` is a non-empty list of {path or handle, kind}")?;
    list.iter()
        .map(|entry| {
            let named = entry["handle"].as_str().or_else(|| entry["path"].as_str());
            let path = named.ok_or("each of `references` needs a `path` or a `handle`")?;
            let word = entry["kind"]
                .as_str()
                .ok_or_else(|| format!("reference {path} needs a `kind`: image, video or audio"))?;
            let kind = Kind::parse(word).ok_or_else(|| {
                format!("reference {path}: unknown kind `{word}` (image, video or audio)")
            })?;
            if path.starts_with(sven_model_drivers::invoke::HANDLE_PREFIX) {
                if !is_handle(path) {
                    return Err(format!(
                        "reference {path} is not a handle of the conversation"
                    ));
                }
                return Ok(Reference {
                    kind,
                    source: ReferenceSource::Handle(path.to_owned()),
                });
            }
            let resolved = file_to_read(workdir, path, "references")?;
            let ext = resolved
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let media_type = kind.media_type(&ext).ok_or_else(|| {
                format!(
                    "reference {path} is not a {} file (by its extension)",
                    kind.name()
                )
            })?;
            Ok(Reference {
                kind,
                source: ReferenceSource::Path {
                    path: resolved,
                    media_type,
                },
            })
        })
        .collect()
}

/// Read the call's arguments into the request body; refuses anything the
/// schema does not name.
pub(crate) fn plan(args: &Value, workdir: &Path) -> Result<Planned, String> {
    let args = args.as_object().ok_or("the arguments must be an object")?;
    let known = [
        "model",
        "prompt",
        "duration",
        "resolution",
        "aspect_ratio",
        "size",
        "generate_audio",
        "seed",
        "edit",
        "action",
        "input_video",
        "references",
        "first_frame",
        "last_frame",
        "params",
        "provider_options",
        "previous_job_id",
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
    if let Some(value) = args.get("size") {
        body.insert("size".into(), json!(size(value)?));
    }
    // A clip of an earlier run to continue, by the job id its answer gave.
    match args.get("previous_job_id") {
        None => {}
        Some(Value::String(id)) if !id.is_empty() && id.len() <= 100 => {
            body.insert("previous_job_id".into(), json!(id));
        }
        Some(_) => {
            return Err("`previous_job_id` is the job id an earlier video's answer gave".into())
        }
    }
    let named = |key: &str| match args.get(key) {
        None => Ok(None),
        Some(Value::String(name)) if !name.is_empty() => Ok(Some(name.as_str())),
        Some(_) => Err(format!("`{key}` must be a name")),
    };
    let (edit, action) = (named("edit")?, named("action")?);
    let input = match args.get("input_video").and_then(Value::as_str) {
        Some(path) => Some(file_to_read(workdir, path, "input_video")?),
        None => None,
    };
    let references = match args.get("references") {
        Some(value) => references(value, workdir)?,
        None => Vec::new(),
    };
    let frames = FRAME_KINDS
        .iter()
        .filter_map(|kind| args.get(*kind).map(|value| frame(value, kind, workdir)))
        .collect::<Result<Vec<_>, String>>()?;
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
    if edit.is_some() && action.is_some() {
        return Err("`edit` and `action` both choose the model's action: give one".into());
    }
    // A provider's own options: the model's options alone (as its listing
    // names them), or under a provider's name. Where they go is decided
    // before sending, by who serves the model (`generate`); the service
    // checks each against the model's listing.
    let mut options = match args.get("provider_options") {
        None => Map::new(),
        Some(Value::Object(options)) => options.clone(),
        Some(_) => {
            return Err("`provider_options` must be an object: the model's options by name".into())
        }
    };
    if options.contains_key("brain") {
        return Err(
            "brain's options are chosen by `action` or `edit`, not `provider_options`".into(),
        );
    }
    match edit.or(action) {
        Some(chosen) => {
            let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("`params` must be an object".into());
            }
            options.insert(
                "brain".into(),
                json!({ "action": chosen, "params": params }),
            );
        }
        None if args.contains_key("params") => {
            return Err("`params` belong to an edit or an action".into());
        }
        None => {}
    }
    if !options.is_empty() {
        body.insert("provider".into(), json!({ "options": options }));
    }
    Ok(Planned {
        body,
        input,
        references,
        frames,
        output,
    })
}

/// A file's bytes, read whole under the size bound, and their digest.
async fn load(path: &Path) -> Result<(Vec<u8>, String), String> {
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
    Ok((bytes, digest))
}

/// A file as a request names it: the service's own file when `path` is an
/// unchanged copy of one it holds (`held`), sent by reference; else its
/// bytes, inline.
async fn inline_or_held(path: &Path, digest: &str, media_type: &str, bytes: &[u8]) -> String {
    match super::held::held_file_id(path, digest).await {
        Some(id) => format!("file:{id}"),
        None => data_url(media_type, bytes),
    }
}

fn data_url(media_type: &str, bytes: &[u8]) -> String {
    format!(
        "data:{media_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

impl Planned {
    /// The body to send, with the files it names read in, and their digests
    /// (kind and content, one per file, in the order sent): the request's
    /// identity must follow what the files hold, not their names.
    ///
    /// An edit's clip is named by the provider's own file when the tool kept
    /// one beside it, else sent inline; references are always inline.
    pub(crate) async fn with_media(&self) -> Result<(Map<String, Value>, Vec<String>), String> {
        let mut body = self.body.clone();
        let mut digests = Vec::new();
        if let Some(path) = &self.input {
            let (bytes, digest) = load(path).await?;
            let stored = tokio::fs::read(sidecar_path(path))
                .await
                .ok()
                .and_then(|raw| serde_json::from_slice::<Sidecar>(&raw).ok())
                .and_then(|s| s.file_id);
            let url = match stored {
                Some(id) => format!("file:{id}"),
                None => {
                    let ext = path.extension().and_then(|e| e.to_str());
                    let media = if ext == Some("webm") {
                        "video/webm"
                    } else {
                        "video/mp4"
                    };
                    data_url(media, &bytes)
                }
            };
            body.insert("input_video".into(), Kind::Video.part(url));
            digests.push(format!("input_video:{digest}"));
        }
        if !self.references.is_empty() {
            let mut parts = Vec::with_capacity(self.references.len());
            for reference in &self.references {
                let url = match &reference.source {
                    ReferenceSource::Path { path, media_type } => {
                        let (bytes, digest) = load(path).await?;
                        digests.push(format!("{}:{digest}", reference.kind.name()));
                        inline_or_held(path, &digest, media_type, &bytes).await
                    }
                    // Named by the handle: the file behind it never changes.
                    ReferenceSource::Handle(handle) => {
                        digests.push(format!("{}:{handle}", reference.kind.name()));
                        handle.clone()
                    }
                };
                parts.push(reference.kind.part(url));
            }
            body.insert("input_references".into(), Value::Array(parts));
        }
        if !self.frames.is_empty() {
            let mut parts = Vec::with_capacity(self.frames.len());
            for frame in &self.frames {
                let url = match &frame.source {
                    FrameSource::Path { path, media_type } => {
                        let (bytes, digest) = load(path).await?;
                        digests.push(format!("{}:{digest}", frame.kind));
                        inline_or_held(path, &digest, media_type, &bytes).await
                    }
                    // Named by the handle: the file behind it never changes.
                    FrameSource::Handle(handle) => {
                        digests.push(format!("{}:{handle}", frame.kind));
                        handle.clone()
                    }
                };
                let mut part = Kind::Image.part(url);
                part["frame_type"] = json!(frame.kind);
                parts.push(part);
            }
            body.insert("frame_images".into(), Value::Array(parts));
        }
        Ok((body, digests))
    }
}

/// The name of a request: the provider, the model and every argument, the
/// files by their content, and the attempt.
pub(crate) fn request_key(
    base_url: &str,
    planned: &Planned,
    digests: &[String],
    attempt: u32,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(base_url.as_bytes());
    hasher.update([0]);
    // serde_json's map is ordered by key, so equal requests serialize equally.
    hasher.update(serde_json::to_vec(&planned.body).unwrap_or_default());
    for digest in digests {
        hasher.update([0]);
        hasher.update(digest.as_bytes());
    }
    if attempt > 0 {
        hasher.update(format!("\0retry {attempt}").as_bytes());
    }
    format!("sven-{}", &hex::encode(hasher.finalize())[..32])
}
