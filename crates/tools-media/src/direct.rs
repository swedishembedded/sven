// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that drive image,
// speech and vision APIs directly for its clients. If your team needs
// expertise in OpenRouter-compatible media APIs then you can procure our
// services by sending an email to info@swedishembedded.com.
//! One invocation straight against a provider's own APIs (a bring-your-own
//! OpenRouter key): `POST /images`, `POST /audio/speech` and
//! `POST /chat/completions`, as the sven service would make them for the
//! agent. What the model makes is written into the working directory and
//! named by its path; there is no conversation to keep it.
//!
//! Every answer is checked: an image's bytes are the raster type they claim,
//! audio is MP3 or raw PCM (wrapped as WAV here), sizes are bounded.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sven_tool_api::tool::{ArtifactRef, ToolOutput, ToolOutputPart};

use super::{file_to_write, shown, MediaConfig, MediaEndpoint};

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The largest answer read.
const MAX_ANSWER_BYTES: usize = 128 * 1024 * 1024;
/// The largest image put in front of the session's model.
const MAX_SHOWN_IMAGE_BYTES: usize = 3 * 1024 * 1024;
/// OpenRouter's raw speech: 16-bit little-endian mono at 24 kHz.
const PCM_RATE: u32 = 24_000;

fn raster(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(("image/png", "png"))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"GIF8") {
        Some(("image/gif", "gif"))
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

/// A WAV container around 16-bit mono PCM.
#[must_use]
pub fn wav_of(pcm: &[u8]) -> Vec<u8> {
    let data = &pcm[..pcm.len() - pcm.len() % 2];
    let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&PCM_RATE.to_le_bytes());
    wav.extend_from_slice(&(PCM_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    wav.extend_from_slice(data);
    wav
}

async fn post(
    endpoint: &MediaEndpoint,
    path: &str,
    body: &Value,
) -> Result<(Vec<u8>, String), String> {
    let response = sven_model_drivers::invoke::http_client()
        .post(format!("{}{path}", endpoint.base_url.trim_end_matches('/')))
        .bearer_auth(&endpoint.api_key)
        .timeout(TIMEOUT)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("the provider could not be reached: {e}"))?;
    let status = response.status();
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_owned())
        .unwrap_or_default();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("reading the answer: {e}"))?;
    if bytes.len() > MAX_ANSWER_BYTES {
        return Err(format!(
            "the answer is larger than {MAX_ANSWER_BYTES} bytes"
        ));
    }
    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|j| j["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("HTTP {}: {message}", status.as_u16()));
    }
    Ok((bytes.to_vec(), mime))
}

fn json_of(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|_| "the answer is not JSON".to_owned())
}

/// The data URLs of the image inputs (inline, as `invocation::request` read them).
fn image_urls(inputs: &[Value]) -> Result<Vec<String>, String> {
    inputs
        .iter()
        .map(|input| {
            let (Some(data), Some(mime)) = (input["data"].as_str(), input["mimeType"].as_str()) else {
                return Err("a provider of its own reads files of the working directory, by path, not handles".to_owned());
            };
            if !mime.starts_with("image/") {
                return Err(format!("{} is not an image", input["name"].as_str().unwrap_or("an input")));
            }
            Ok(format!("data:{mime};base64,{data}"))
        })
        .collect()
}

/// An image run as the provider's `/images` request: the parameters as
/// given, and one provider's own options (`provider_options`) sent to that
/// provider only, as the router reads them.
fn image_request(
    body: &Value,
    mut params: serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    if let Some(given) = params.remove("provider_options") {
        let options = given
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or("`provider_options` names one provider, whose options go to it")?;
        let (slug, own) = options.iter().next().expect("one provider");
        if !own.is_object() {
            return Err(format!(
                "`provider_options.{slug}` must be an object of that provider's options"
            ));
        }
        params.insert(
            "provider".into(),
            json!({"only": [slug], "options": {slug: own}}),
        );
    }
    params.insert("model".into(), body["model"].clone());
    params.insert("prompt".into(), body["prompt"].clone());
    Ok(params)
}

/// A speech run as the provider's `/audio/speech` request: the prompt, or
/// several speakers' `turns`; a voice to clone from sound inputs (at most
/// three, with `reference_transcript`) and a picture of the scene.
fn speech_request(
    body: &Value,
    params: &serde_json::Map<String, Value>,
    inputs: &[Value],
) -> Result<Value, String> {
    let mut request = params.clone();
    let format = request
        .remove("format")
        .and_then(|f| f.as_str().map(str::to_owned))
        .unwrap_or_else(|| "mp3".into());
    let said = request
        .remove("turns")
        .unwrap_or_else(|| body["prompt"].clone());
    let transcript = request.remove("reference_transcript");
    let mut references = Vec::new();
    for input in inputs {
        let (Some(data), Some(mime)) = (input["data"].as_str(), input["mimeType"].as_str()) else {
            return Err(
                "a provider of its own reads files of the working directory, by path, not handles"
                    .to_owned(),
            );
        };
        if mime.starts_with("audio/") {
            references.push(json!({"type": "input_audio", "input_audio": {"data": format!("data:{mime};base64,{data}")}}));
        } else if mime.starts_with("image/") {
            references.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}}));
        } else {
            return Err(format!(
                "{} is not a sound or a picture to voice from",
                input["name"].as_str().unwrap_or("an input")
            ));
        }
    }
    if references
        .iter()
        .filter(|r| r["type"] == "input_audio")
        .count()
        > 3
    {
        return Err("a voice is cloned from at most three sounds".into());
    }
    if let Some(text) = transcript {
        if references.is_empty() {
            return Err("a transcript goes with a voice sample given as an input".into());
        }
        references.push(json!({"type": "text", "text": text}));
    }
    request.insert("model".into(), body["model"].clone());
    request.insert("input".into(), said);
    request.insert("response_format".into(), Value::String(format));
    if !references.is_empty() {
        request.insert("input_references".into(), Value::Array(references));
    }
    Ok(Value::Object(request))
}

/// The chat parameters a provider's own `/chat/completions` is sent, as
/// `run_model` names them; any other is refused, never dropped.
const CHAT_PARAMS: [&str; 9] = [
    "system",
    "max_tokens",
    "temperature",
    "top_p",
    "seed",
    "stop",
    "response_format",
    "reasoning",
    "pdf_engine",
];

/// A chat run as the provider's `/chat/completions` request: the prompt with
/// each input as its part (an image, a PDF, a sound, a video), the system
/// message, and every parameter given.
fn chat_request(
    body: &Value,
    params: &serde_json::Map<String, Value>,
    inputs: &[Value],
) -> Result<Value, String> {
    if let Some(unknown) = params.keys().find(|k| !CHAT_PARAMS.contains(&k.as_str())) {
        return Err(format!(
            "a chat run takes {}, not `{unknown}`",
            CHAT_PARAMS.join(", ")
        ));
    }
    let mut content = vec![json!({"type": "text", "text": body["prompt"]})];
    for input in inputs {
        let (Some(data), Some(mime)) = (input["data"].as_str(), input["mimeType"].as_str()) else {
            return Err(
                "a provider of its own reads files of the working directory, by path, not handles"
                    .to_owned(),
            );
        };
        let name = input["name"].as_str().unwrap_or("input");
        content.push(match mime {
            m if m.starts_with("image/") => json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}}),
            "application/pdf" => json!({"type": "file", "file": {"filename": name, "file_data": format!("data:{mime};base64,{data}")}}),
            m if m.starts_with("video/") => json!({"type": "video_url", "video_url": {"url": format!("data:{mime};base64,{data}")}}),
            "audio/wav" => json!({"type": "input_audio", "input_audio": {"data": data, "format": "wav"}}),
            "audio/mpeg" => json!({"type": "input_audio", "input_audio": {"data": data, "format": "mp3"}}),
            "audio/flac" => json!({"type": "input_audio", "input_audio": {"data": data, "format": "flac"}}),
            "audio/ogg" => json!({"type": "input_audio", "input_audio": {"data": data, "format": "ogg"}}),
            "audio/mp4" => json!({"type": "input_audio", "input_audio": {"data": data, "format": "m4a"}}),
            _ => return Err(format!("{name} ({mime}) is not an image, a PDF, a sound or a video a chat model reads")),
        });
    }
    let mut messages = Vec::new();
    if let Some(system) = params.get("system") {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.push(json!({"role": "user", "content": content}));
    let mut request = json!({"model": body["model"], "messages": messages});
    for key in [
        "max_tokens",
        "temperature",
        "top_p",
        "seed",
        "stop",
        "response_format",
        "reasoning",
    ] {
        if let Some(value) = params.get(key) {
            request[key] = value.clone();
        }
    }
    if let Some(engine) = params.get("pdf_engine") {
        request["plugins"] = json!([{"id": "file-parser", "pdf": {"engine": engine}}]);
    }
    Ok(request)
}

/// What one direct call made: its text, and its files as bytes with type
/// and extension.
struct Made {
    text: Option<String>,
    files: Vec<(Vec<u8>, &'static str, &'static str)>,
    finish_reason: Option<String>,
}

async fn call(endpoint: &MediaEndpoint, body: &Value) -> Result<Made, String> {
    let params = body["params"].as_object().cloned().unwrap_or_default();
    let inputs = body["inputs"].as_array().cloned().unwrap_or_default();
    match body["operation"].as_str().unwrap_or("") {
        "image" => {
            let mut request = image_request(body, params)?;
            let references = image_urls(&inputs)?;
            if !references.is_empty() {
                request.insert(
                    "input_references".into(),
                    Value::Array(references.into_iter().map(|url| json!({"type": "image_url", "image_url": {"url": url}})).collect()),
                );
            }
            let (bytes, _) = post(endpoint, "/images", &Value::Object(request)).await?;
            let answer = json_of(&bytes)?;
            let files = answer["data"]
                .as_array()
                .map(|data| {
                    data.iter()
                        .filter_map(|image| image["b64_json"].as_str())
                        .map(|b64| {
                            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| "an image is not base64".to_owned())?;
                            let (mime, ext) = raster(&bytes).ok_or("the provider sent an image that is not one")?;
                            Ok((bytes, mime, ext))
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
                .transpose()?
                .unwrap_or_default();
            Ok(Made { text: None, files, finish_reason: None })
        }
        "speech" => {
            let format = params.get("format").and_then(Value::as_str).unwrap_or("mp3").to_owned();
            let request = speech_request(body, &params, &inputs)?;
            let (bytes, _) = post(endpoint, "/audio/speech", &request).await?;
            let file = if format == "pcm" {
                (wav_of(&bytes), "audio/wav", "wav")
            } else if bytes.starts_with(b"ID3") || (bytes.len() > 1 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0) {
                (bytes, "audio/mpeg", "mp3")
            } else {
                return Err("the provider sent audio that is not MP3".into());
            };
            Ok(Made { text: None, files: vec![file], finish_reason: None })
        }
        "chat" => {
            let request = chat_request(body, &params, &inputs)?;
            let (bytes, _) = post(endpoint, "/chat/completions", &request).await?;
            let answer = json_of(&bytes)?;
            Ok(Made {
                text: answer["choices"][0]["message"]["content"].as_str().map(str::to_owned),
                files: Vec::new(),
                finish_reason: answer["choices"][0]["finish_reason"].as_str().map(str::to_owned),
            })
        }
        other => Err(format!("`{other}` runs only through the sven service; a provider of its own runs chat, image and speech")),
    }
}

/// Run `body` (an invocation's JSON) straight against `endpoint`, writing what
/// it makes into the media directory (or `output`).
pub(crate) async fn run(
    endpoint: &MediaEndpoint,
    config: &MediaConfig,
    workdir: &Path,
    call_id: &str,
    body: &Value,
    output: Option<&str>,
) -> ToolOutput {
    let mut made = match call(endpoint, body).await {
        Ok(made) => made,
        Err(error) => return ToolOutput::err(call_id, format!("The model did not run: {error}")),
    };
    // A chat answer asked for as a file is one, written here like the files
    // other operations make; its text is not answered.
    let kept_as = body["delivery"]["mimeType"].as_str();
    if let Some(mime) = kept_as {
        let (mime, ext) = match mime {
            "application/json" => ("application/json", "json"),
            "text/plain" => ("text/plain", "txt"),
            _ => ("text/markdown", "md"),
        };
        if let Some(answer) = made.text.take().filter(|t| !t.trim().is_empty()) {
            made.files.push((answer.into_bytes(), mime, ext));
        }
    }
    let ending = super::invocation::ending_note(made.finish_reason.as_deref(), &[]);
    let stem =
        hex::encode(Sha256::digest(format!("{call_id}\0{body}").as_bytes()))[..12].to_owned();
    let mut text = made
        .text
        .filter(|t| !t.is_empty())
        .map(|t| format!("{t}\n\n"))
        .unwrap_or_default();
    let mut artifacts = Vec::new();
    let mut images = Vec::new();
    for (index, (bytes, mime, ext)) in made.files.into_iter().enumerate() {
        let name = format!(
            "{}-{}.{ext}",
            if mime.starts_with("audio/") {
                "audio"
            } else if mime.starts_with("image/") {
                "image"
            } else {
                "answer"
            },
            index + 1
        );
        let relative = match output.filter(|_| index == 0) {
            Some(output) => output.to_owned(),
            None => config
                .dir
                .join(format!("{stem}-{name}"))
                .to_string_lossy()
                .into_owned(),
        };
        let target = match file_to_write(workdir, &relative, "output") {
            Ok(target) => target,
            Err(error) => return ToolOutput::err(call_id, error),
        };
        if let Err(error) = std::fs::write(&target, &bytes) {
            return ToolOutput::err(call_id, format!("writing {}: {error}", target.display()));
        }
        let artifact = ArtifactRef {
            uri: shown(workdir, &target),
            name,
            mime_type: mime.to_owned(),
            size: bytes.len() as u64,
        };
        text.push_str(&format!(
            "Made `{}` ({mime}, {} bytes): {}\n",
            artifact.name, artifact.size, artifact.uri
        ));
        if mime.starts_with("image/") && bytes.len() <= MAX_SHOWN_IMAGE_BYTES {
            images.push(format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            ));
        }
        artifacts.push(artifact);
    }
    if !artifacts.is_empty() {
        text.push_str("Pass a path in `inputs` to work on one.\n");
    }
    text.push_str(&ending);
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
    fn a_chat_run_sends_every_parameter_and_each_input_as_its_part() {
        let body = json!({"model": "m", "prompt": "read it"});
        let params = json!({"system": "be brief", "temperature": 0.2, "top_p": 0.9, "seed": 7, "max_tokens": 50,
                            "response_format": {"type": "json_object"}, "reasoning": {"effort": "low"}, "pdf_engine": "pdf-text"});
        let inputs = [
            json!({"data": "JVBERi0=", "mimeType": "application/pdf", "name": "a.pdf"}),
            json!({"data": "UklGRg==", "mimeType": "audio/wav", "name": "a.wav"}),
        ];
        let request = chat_request(&body, params.as_object().unwrap(), &inputs).unwrap();
        assert_eq!(
            request["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(request["messages"][1]["content"][1]["type"], "file");
        assert_eq!(
            request["messages"][1]["content"][2],
            json!({"type": "input_audio", "input_audio": {"data": "UklGRg==", "format": "wav"}})
        );
        for key in [
            "temperature",
            "top_p",
            "seed",
            "max_tokens",
            "response_format",
            "reasoning",
        ] {
            assert_eq!(request[key], params[key], "{key}");
        }
        assert_eq!(
            request["plugins"],
            json!([{"id": "file-parser", "pdf": {"engine": "pdf-text"}}])
        );
        // A parameter the provider's chat would not take is refused, not dropped.
        let unknown = json!({"frequency": 1});
        assert!(chat_request(&body, unknown.as_object().unwrap(), &[])
            .unwrap_err()
            .contains("`frequency`"));
        let zip = [json!({"data": "UEs=", "mimeType": "application/zip", "name": "a.zip"})];
        assert!(chat_request(&body, &serde_json::Map::new(), &zip)
            .unwrap_err()
            .contains("a.zip"));
    }

    #[test]
    fn a_speech_run_voices_turns_in_a_voice_cloned_from_a_sound() {
        let body = json!({"model": "fish/s2", "prompt": "the opening"});
        let params = json!({"format": "pcm", "turns": [{"text": "Hi", "voice": "a"}, {"text": "Hey", "voice": "b"}], "reference_transcript": "sample words"});
        let inputs = [json!({"data": "UklGRg==", "mimeType": "audio/wav", "name": "v.wav"})];
        let request = speech_request(&body, params.as_object().unwrap(), &inputs).unwrap();
        assert_eq!(request["input"], params["turns"]);
        assert_eq!(request["response_format"], "pcm");
        assert_eq!(
            request["input_references"],
            json!([{"type": "input_audio", "input_audio": {"data": "data:audio/wav;base64,UklGRg=="}}, {"type": "text", "text": "sample words"}])
        );
        assert!(request.get("turns").is_none() && request.get("reference_transcript").is_none());
        let pdf = [json!({"data": "JVBERi0=", "mimeType": "application/pdf", "name": "a.pdf"})];
        assert!(speech_request(&body, &serde_json::Map::new(), &pdf)
            .unwrap_err()
            .contains("a.pdf"));
    }

    #[test]
    fn an_image_run_sends_one_provider_s_options_to_that_provider_only() {
        let body = json!({"model": "openai/gpt-image-2", "prompt": "a whale"});
        let params =
            json!({"aspect_ratio": "1:1", "provider_options": {"openai": {"moderation": "low"}}});
        let request = image_request(&body, params.as_object().unwrap().clone()).unwrap();
        assert_eq!(
            request["provider"],
            json!({"only": ["openai"], "options": {"openai": {"moderation": "low"}}})
        );
        assert_eq!(request["aspect_ratio"], "1:1");
        assert!(request.get("provider_options").is_none());
        let two = json!({"provider_options": {"a": {}, "b": {}}});
        assert!(image_request(&body, two.as_object().unwrap().clone()).is_err());
    }

    #[test]
    fn raw_speech_becomes_a_wav_that_plays() {
        let wav = wav_of(&[0u8; 481]);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().unwrap()),
            PCM_RATE
        );
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 480);
    }
}
