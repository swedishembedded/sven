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

#[derive(Debug, Clone, PartialEq)]
struct Reference {
    kind: Kind,
    path: PathBuf,
    media_type: &'static str,
}

/// The request a call makes, without the files it names (they are read when
/// the request is sent and named by their digests), and where the clip is
/// also to be saved.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Planned {
    pub(crate) body: Map<String, Value>,
    input: Option<PathBuf>,
    references: Vec<Reference>,
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
        .ok_or("`references` is a non-empty list of {path, kind}")?;
    list.iter()
        .map(|entry| {
            let path = entry["path"]
                .as_str()
                .ok_or("each of `references` needs a `path`")?;
            let word = entry["kind"]
                .as_str()
                .ok_or_else(|| format!("reference {path} needs a `kind`: image, video or audio"))?;
            let kind = Kind::parse(word).ok_or_else(|| {
                format!("reference {path}: unknown kind `{word}` (image, video or audio)")
            })?;
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
                path: resolved,
                media_type,
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
    if let Some(value) = args.get("size") {
        body.insert("size".into(), json!(size(value)?));
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
    match edit.or(action) {
        Some(chosen) => {
            let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("`params` must be an object".into());
            }
            body.insert(
                "provider".into(),
                json!({ "options": { "brain": { "action": chosen, "params": params } } }),
            );
        }
        None if args.contains_key("params") => {
            return Err("`params` belong to an edit or an action".into());
        }
        None => {}
    }
    Ok(Planned {
        body,
        input,
        references,
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
                let (bytes, digest) = load(&reference.path).await?;
                parts.push(reference.kind.part(data_url(reference.media_type, &bytes)));
                digests.push(format!("{}:{digest}", reference.kind.name()));
            }
            body.insert("input_references".into(), Value::Array(parts));
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
