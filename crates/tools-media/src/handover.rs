// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that hand pictures,
// recordings and other files to sub-agents for its clients. If your team
// needs expertise in multi-agent orchestration then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Files handed to a sub-agent: each named in its first message by the handle
//! or path it can pass on, and each image it can see attached as an image.
//! Nothing is copied into the service; a handle stays the same file.

use std::path::Path;

use base64::Engine as _;
use sven_model_drivers::invoke::HANDLE_PREFIX;

use super::{file_to_read, invoker, MediaConfig, MediaEndpoint};

/// The largest image attached to a sub-agent's first message.
const MAX_HANDED_IMAGE_BYTES: u64 = 3 * 1024 * 1024;
/// The most files one sub-agent is handed.
const MAX_HANDED: usize = 16;

const RASTER: [(&str, &str); 5] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// The largest file fetched into the working directory by handle.
const MAX_FETCHED_BYTES: u64 = 50 * 1024 * 1024;

/// Fetch file `handle` into `workdir`, under `.sven/files/`, named by its id
/// and type; answers the path relative to `workdir`. A file fetched before is
/// answered as it is.
///
/// # Errors
/// Not a handle, unreachable or refused, too large, or not writable.
pub async fn fetch_to(
    endpoint: &MediaEndpoint,
    config: &MediaConfig,
    workdir: &Path,
    handle: &str,
) -> Result<String, String> {
    let id = handle
        .strip_prefix(HANDLE_PREFIX)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .ok_or("not a handle of a file the service keeps")?;
    let (bytes, mime) = invoker(endpoint, config)
        .download(handle, MAX_FETCHED_BYTES)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let extension = match mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        _ => "bin",
    };
    let relative = format!(".sven/files/{id}.{extension}");
    let target = super::file_to_write(workdir, &relative, "path")?;
    std::fs::write(&target, bytes).map_err(|e| format!("saving {relative}: {e}"))?;
    Ok(relative)
}

/// What a sub-agent is handed.
#[derive(Debug, Default)]
pub struct Handover {
    /// The lines that name each file, for the end of its prompt.
    pub note: String,
    /// The images among them, as `(media type, base64)`.
    pub images: Vec<(String, String)>,
}

/// `inputs` (handles or paths of `workdir`) handed to a sub-agent, its images
/// fetched (a handle, through `media`) or read (a path).
///
/// # Errors
/// More inputs than one sub-agent takes, a path outside `workdir` or missing,
/// or a handle with no service to fetch it from.
pub async fn hand_over(
    media: Option<(&MediaEndpoint, &MediaConfig)>,
    workdir: &Path,
    inputs: &[String],
) -> Result<Handover, String> {
    if inputs.len() > MAX_HANDED {
        return Err(format!("`inputs` names at most {MAX_HANDED} files"));
    }
    let mut handed = Handover::default();
    for input in inputs {
        if input.starts_with(HANDLE_PREFIX) {
            let (endpoint, config) = media.ok_or(
                "a handle names a file of the sven service, which this session cannot reach",
            )?;
            let fetched = invoker(endpoint, config)
                .download(input, MAX_HANDED_IMAGE_BYTES)
                .await;
            match fetched {
                Ok((bytes, mime)) if RASTER.iter().any(|(_, m)| *m == mime) => {
                    handed.images.push((
                        mime.clone(),
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ));
                    handed
                        .note
                        .push_str(&format!("\n- {input} ({mime}; it is attached as an image)"));
                }
                Ok((_, mime)) => handed.note.push_str(&format!("\n- {input} ({mime})")),
                // Too large to attach, or not an image: it is still named.
                Err(_) => handed.note.push_str(&format!("\n- {input}")),
            }
            continue;
        }
        let path = file_to_read(workdir, input, "inputs")?;
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let image = RASTER
            .iter()
            .find(|(ext, _)| Some(*ext) == extension.as_deref())
            .map(|(_, mime)| *mime);
        let size = std::fs::metadata(&path)
            .map_err(|e| format!("`inputs` {input}: {e}"))?
            .len();
        match image.filter(|_| size <= MAX_HANDED_IMAGE_BYTES) {
            Some(mime) => {
                let bytes = std::fs::read(&path).map_err(|e| format!("`inputs` {input}: {e}"))?;
                handed.images.push((
                    mime.to_owned(),
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                ));
                handed.note.push_str(&format!(
                    "\n- `{input}` ({mime}; it is attached as an image)"
                ));
            }
            None => handed
                .note
                .push_str(&format!("\n- `{input}` ({size} bytes)")),
        }
    }
    if !handed.note.is_empty() {
        handed.note = format!(
            "\n\nFiles you were handed (pass one on to a model by this handle or path, as it is):{}",
            handed.note
        );
    }
    Ok(handed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{handle_of, serve, ONE_PIXEL_PNG};

    #[tokio::test]
    async fn a_file_is_fetched_by_handle_into_the_working_directory() {
        let fake = serve(0).await;
        let dir = tempfile::tempdir().unwrap();
        let endpoint = MediaEndpoint {
            base_url: fake.base_url.clone(),
            api_key: "k".into(),
        };
        let saved = fetch_to(
            &endpoint,
            &MediaConfig::default(),
            dir.path(),
            &handle_of(3),
        )
        .await
        .unwrap();
        assert_eq!(
            saved,
            ".sven/files/00000000-0000-4000-8000-000000000003.png"
        );
        assert_eq!(
            std::fs::read(dir.path().join(&saved)).unwrap(),
            ONE_PIXEL_PNG
        );
        assert!(fetch_to(
            &endpoint,
            &MediaConfig::default(),
            dir.path(),
            "mcp-file://files/../x"
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn a_sub_agent_is_handed_each_file_by_handle_and_sees_the_images() {
        let fake = serve(0).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        std::fs::write(dir.path().join("car.png"), ONE_PIXEL_PNG).unwrap();
        let endpoint = MediaEndpoint {
            base_url: fake.base_url.clone(),
            api_key: "k".into(),
        };
        let config = MediaConfig {
            session: Some("conv-1".into()),
            ..MediaConfig::default()
        };
        let inputs = [handle_of(3), "car.png".to_owned(), "notes.txt".to_owned()];
        let handed = hand_over(Some((&endpoint, &config)), dir.path(), &inputs)
            .await
            .unwrap();
        assert_eq!(handed.images.len(), 2);
        assert!(handed.images.iter().all(|(mime, _)| mime == "image/png"));
        for input in &inputs {
            assert!(handed.note.contains(input.as_str()), "{}", handed.note);
        }
        assert!(hand_over(None, dir.path(), &[handle_of(3)]).await.is_err());
        assert!(hand_over(None, dir.path(), &["../x.png".to_owned()])
            .await
            .is_err());
    }
}
