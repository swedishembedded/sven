// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that combine image,
// speech, vision and music models for its clients. If your team needs
// expertise in multimodal agent tooling then you can procure our services by
// sending an email to info@swedishembedded.com.
//! One invocation of any model but video (`run_model` with `operation`
//! chat, image, speech, ...), and what the agent and the person get back:
//! the model's text, and every file it made by handle. A chat answer asked
//! for as a file (`keep_as`: a page's OCR, extracted fields) is kept by the
//! service and answered by handle like any other file, so its text never
//! reaches the session's own model unless the agent reads it.
//!
//! Inside a conversation of the service the files stay there: the tool
//! answers their handles (`mcp-file://files/<id>`), the person sees them in
//! the conversation, and another model reads them by the same handle.
//! Without one they are fetched into the media directory and answered as
//! paths. A raster image is also put in front of the session's own model,
//! so "make it darker" refers to what it saw; audio is not (a model rarely
//! needs to hear what it had said, and it is costly).

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sven_model_drivers::invoke::{InvocationClient, Invoked, MadeFile, HANDLE_PREFIX};
use sven_tool_api::tool::{ArtifactRef, ToolOutput, ToolOutputPart};

use super::{file_to_read, file_to_write, shown, MediaConfig};

/// The largest file sent inline as an input (the service's own limit).
const MAX_INLINE_BYTES: u64 = 15 * 1024 * 1024;
/// The largest image put in front of the session's model.
const MAX_SHOWN_IMAGE_BYTES: u64 = 3 * 1024 * 1024;
/// The largest file fetched into the media directory.
const MAX_FETCHED_BYTES: u64 = 512 * 1024 * 1024;

/// The images a model can be shown.
const RASTER: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// A type for a file the agent names by path, from its extension; the
/// service checks the bytes again.
fn type_of(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("mp4") => "video/mp4",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Each input as the service takes it: a handle as it is, a file of the
/// working directory read and sent inline.
fn inputs(workdir: &Path, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
    let Some(listed) = args.get("inputs") else {
        return Ok(Vec::new());
    };
    let listed = listed
        .as_array()
        .ok_or("`inputs` is a list of handles or paths")?;
    listed
        .iter()
        .map(|input| {
            let input = input
                .as_str()
                .ok_or("`inputs` holds handles (mcp-file://...) or paths")?;
            if input.starts_with(HANDLE_PREFIX) {
                return Ok(json!({ "uri": input }));
            }
            let path = file_to_read(workdir, input, "inputs")?;
            let size = std::fs::metadata(&path)
                .map_err(|e| format!("`inputs` {input}: {e}"))?
                .len();
            if size > MAX_INLINE_BYTES {
                return Err(format!(
                    "`inputs` {input} is over {MAX_INLINE_BYTES} bytes, more than a model takes"
                ));
            }
            let bytes = std::fs::read(&path).map_err(|e| format!("`inputs` {input}: {e}"))?;
            Ok(json!({
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                "mimeType": type_of(&path),
                "name": path.file_name().and_then(|n| n.to_str()).unwrap_or("input"),
            }))
        })
        .collect()
}

/// The idempotency key of call `call_id` asking `body` in `session`: the
/// same call asked again (a turn resumed after a restart) is the same
/// invocation and is charged once; another call is another invocation.
fn idempotency_key(session: Option<&str>, call_id: &str, body: &Value) -> String {
    let digest = Sha256::new()
        .chain_update(session.unwrap_or("local"))
        .chain_update([0])
        .chain_update(call_id)
        .chain_update([0])
        .chain_update(body.to_string());
    format!("sven-{}", &hex::encode(digest.finalize())[..48])
}

/// The JSON body of an invocation from `run_model`'s arguments.
pub(crate) fn request(
    workdir: &Path,
    args: &Map<String, Value>,
    operation: &str,
) -> Result<Value, String> {
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or("`model` is required")?;
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .ok_or("`prompt` is required")?;
    let params = match args.get("params") {
        None => json!({}),
        Some(Value::Object(params)) => Value::Object(params.clone()),
        Some(_) => return Err("`params` is an object of the operation's parameters".into()),
    };
    let mut body = json!({
        "model": model,
        "operation": operation,
        "prompt": prompt,
        "inputs": inputs(workdir, args)?,
        "params": params,
    });
    // What the service keeps the files under: the name given, else the name
    // of the file the agent saves its copy as; absent, the service names it
    // from the prompt.
    let named = args
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            args.get("output")
                .and_then(Value::as_str)
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        });
    match keep_as(args, operation)? {
        // A chat answer kept as a file is named in its delivery.
        Some(mime) => {
            let mut delivery = json!({ "mode": "artifact", "mimeType": mime });
            if let Some(name) = named {
                delivery["name"] = Value::String(name);
            }
            body["delivery"] = delivery;
        }
        None => {
            if let Some(name) = named {
                body["name"] = Value::String(name);
            }
        }
    }
    Ok(body)
}

/// The type a chat answer is kept as a file of (`keep_as`), or `None` when
/// it is answered as text.
pub(crate) fn keep_as(
    args: &Map<String, Value>,
    operation: &str,
) -> Result<Option<&'static str>, String> {
    let Some(kind) = args.get("keep_as") else {
        return Ok(None);
    };
    if operation != "chat" {
        return Err(
            "`keep_as` keeps a chat answer as a file; other operations make files of their own"
                .into(),
        );
    }
    match kind.as_str() {
        Some("markdown") => Ok(Some("text/markdown")),
        Some("json") => Ok(Some("application/json")),
        Some("text") => Ok(Some("text/plain")),
        _ => Err("`keep_as` is one of markdown, json or text".into()),
    }
}

/// What the agent is told about how the model's answer ended, when there is
/// something to tell.
pub(crate) fn ending_note(finish_reason: Option<&str>, warnings: &[String]) -> String {
    let mut note = String::new();
    if finish_reason == Some("length") {
        note.push_str(
            "The answer was cut off at the model's output limit: ask for less at once (fewer \
             pages, part of a page) or use a model with a larger limit.\n",
        );
    }
    for warning in warnings {
        note.push_str(&format!("Note: {warning}.\n"));
    }
    note
}

/// A file made, put where the agent can find it: by handle in a
/// conversation, else fetched under the media directory (or at `output`).
async fn place(
    client: &InvocationClient,
    config: &MediaConfig,
    workdir: &Path,
    invoked: &Invoked,
    made: &MadeFile,
    output: Option<&str>,
) -> Result<(ArtifactRef, Option<Vec<u8>>), String> {
    let raster = RASTER.contains(&made.mime_type.as_str()) && made.size <= MAX_SHOWN_IMAGE_BYTES;
    if client.in_conversation() {
        let artifact = ArtifactRef {
            uri: made.uri.clone(),
            name: made.name.clone(),
            mime_type: made.mime_type.clone(),
            size: made.size,
        };
        // Fetched only to show the session's model, and only an image.
        let bytes = if raster {
            client
                .download(&made.uri, MAX_SHOWN_IMAGE_BYTES)
                .await
                .ok()
                .map(|(b, _)| b)
        } else {
            None
        };
        return Ok((artifact, bytes));
    }
    let (bytes, _) = client
        .download(&made.uri, MAX_FETCHED_BYTES)
        .await
        .map_err(|e| format!("fetching {}: {e:#}", made.name))?;
    let relative: PathBuf = match output {
        Some(output) => PathBuf::from(output),
        None => config.dir.join(format!(
            "{}-{}",
            invoked
                .id
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(12)
                .collect::<String>(),
            made.name
        )),
    };
    let target = file_to_write(workdir, &relative.to_string_lossy(), "output")?;
    std::fs::write(&target, &bytes).map_err(|e| format!("writing {}: {e}", target.display()))?;
    let artifact = ArtifactRef {
        uri: shown(workdir, &target),
        name: made.name.clone(),
        mime_type: made.mime_type.clone(),
        size: bytes.len() as u64,
    };
    Ok((artifact, raster.then_some(bytes)))
}

/// Run the invocation `body` for call `call_id` and answer what it made.
pub(crate) async fn run(
    client: &InvocationClient,
    config: &MediaConfig,
    workdir: &Path,
    call_id: &str,
    body: &Value,
    output: Option<&str>,
) -> ToolOutput {
    let key = idempotency_key(config.session.as_deref(), call_id, body);
    let invoked = match client.invoke(body, &key).await {
        Ok(invoked) => invoked,
        Err(error) => return ToolOutput::err(call_id, format!("The model did not run: {error:#}")),
    };
    let mut text = String::new();
    if let Some(answer) = invoked.text.as_deref().filter(|t| !t.is_empty()) {
        text.push_str(answer);
        text.push_str("\n\n");
    }
    let mut artifacts = Vec::new();
    let mut images = Vec::new();
    for (index, made) in invoked.outputs.iter().enumerate() {
        // `output` names where the first file goes; the rest keep their names.
        let wanted = if index == 0 { output } else { None };
        match place(client, config, workdir, &invoked, made, wanted).await {
            Ok((artifact, bytes)) => {
                if let Some(bytes) = bytes {
                    images.push(format!(
                        "data:{};base64,{}",
                        artifact.mime_type,
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    ));
                }
                text.push_str(&format!(
                    "Made `{}` ({}, {} bytes): {}\n",
                    artifact.name, artifact.mime_type, artifact.size, artifact.uri
                ));
                artifacts.push(artifact);
            }
            Err(error) => text.push_str(&format!(
                "Made {} but could not keep it here: {error}\n",
                made.name
            )),
        }
    }
    if invoked.outputs.is_empty() && invoked.text.is_none() {
        text.push_str("The model answered nothing.\n");
    }
    text.push_str(&ending_note(
        invoked.finish_reason.as_deref(),
        &invoked.warnings,
    ));
    if !artifacts.is_empty() {
        text.push_str(if client.in_conversation() {
            "They are shown in the conversation; pass a handle in `inputs` to work on one.\n"
        } else {
            "Pass a path in `inputs` to work on one.\n"
        });
    }
    let text = text.trim_end().to_owned();
    let mut parts = vec![ToolOutputPart::Text(text.clone())];
    parts.extend(images.into_iter().map(ToolOutputPart::Image));
    let mut out = ToolOutput::with_parts(call_id, parts);
    out.content = text;
    out.with_artifacts(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_it_makes_is_named_as_the_agent_named_it() {
        let dir = tempfile::tempdir().unwrap();
        let ask = |extra: Value| {
            let mut args = json!({ "model": "m", "prompt": "a red car" });
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            request(dir.path(), args.as_object().unwrap(), "image").unwrap()
        };
        // The file it saves its copy as is the name the service keeps too.
        assert_eq!(
            ask(json!({ "output": "shots/sven-slap-1.png" }))["name"],
            "sven-slap-1.png"
        );
        // A name given wins.
        assert_eq!(
            ask(json!({ "output": "a.png", "name": "Poster, final" }))["name"],
            "Poster, final"
        );
        // Neither: the service names it from the prompt.
        assert!(ask(json!({})).get("name").is_none());
    }

    #[test]
    fn the_same_call_is_one_invocation_and_another_call_another() {
        let body = json!({ "model": "m", "prompt": "a red car" });
        let once = idempotency_key(Some("s"), "call_1", &body);
        assert_eq!(once, idempotency_key(Some("s"), "call_1", &body));
        assert_ne!(once, idempotency_key(Some("s"), "call_2", &body));
        assert_ne!(once, idempotency_key(Some("t"), "call_1", &body));
        assert!(once.len() <= 255 && once.bytes().all(|b| b.is_ascii_graphic()));
    }

    #[test]
    fn inputs_are_handles_as_they_are_and_files_of_the_working_directory_inline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("car.png"), b"png").unwrap();
        let args = json!({ "inputs": ["mcp-file://files/7b1d", "car.png"] });
        let sent = inputs(dir.path(), args.as_object().unwrap()).unwrap();
        assert_eq!(sent[0], json!({ "uri": "mcp-file://files/7b1d" }));
        assert_eq!(
            sent[1],
            json!({ "data": "cG5n", "mimeType": "image/png", "name": "car.png" })
        );
        let outside = json!({ "inputs": ["../secret.png"] });
        assert!(inputs(dir.path(), outside.as_object().unwrap()).is_err());
    }
}
