// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for reproducible, resumable video
// generation for its clients. If your team needs expertise in generative
// media pipelines then you can procure our services by sending an email to
// info@swedishembedded.com.
//! One generation, as the `run_model` tool and `sven media generate` both
//! make it: the request planned and named by what it asks, submitted under
//! that name, polled to its end, and the clip written under the same name.
//!
//! Nothing but the name is kept between runs, which is enough to resume: a
//! process that dies while polling asks again under the same idempotency key
//! and the server answers the job it already runs (a server that keeps no
//! jobs across its own restart renders again); once the clip is written, the
//! file answers and nothing is sent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sven_model_drivers::video::{VideoClient, VideoState, VideoStatus};

use super::request::{plan, request_key};
use super::{client, shown, sidecar_path, MediaConfig, MediaEndpoint, Sidecar};

/// How often a running job is asked about.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// Failed attempts at the same request before the generation gives up.
const MAX_ATTEMPTS: u32 = 3;

/// A clip delivered, or found already made.
#[derive(Debug, Clone)]
pub struct Generated {
    /// Where the clip is for the caller: the `output` path when one was
    /// asked for, else the content-named file under the media directory.
    pub path: PathBuf,
    /// [`Self::path`] as the caller reads it: relative to the working
    /// directory.
    pub shown: String,
    /// The clip was made by an earlier run of the same request: nothing was
    /// submitted.
    pub reused: bool,
    /// The bytes downloaded; none when reused.
    pub bytes: Option<u64>,
    /// The seconds of video the provider reports.
    pub seconds: Option<f64>,
    /// What the provider charged, when it says.
    pub cost: Option<f64>,
    /// The provider's own stored file of the clip, when it keeps one (the
    /// sven service: a file of the conversation the job was for).
    pub file_id: Option<String>,
    /// The job that made it, which a later request names to continue it
    /// (`previous_job_id`); none for a clip made before jobs were kept.
    pub job_id: Option<String>,
}

impl Generated {
    /// The SHA-256 of the clip, in hex.
    ///
    /// # Errors
    /// The clip cannot be read.
    pub fn digest(&self) -> std::io::Result<String> {
        Ok(hex::encode(Sha256::digest(std::fs::read(&self.path)?)))
    }
}

/// Providers that take their options nested in one object, not under their
/// name directly: Google's (the video API's documentation: "Google Veo
/// models are the only video models that nest options in a `parameters`
/// object").
const NESTED_UNDER: [(&str, &str); 2] = [
    ("google-vertex", "parameters"),
    ("google-ai-studio", "parameters"),
];

/// Where the model's own options given alone go. A provider ignores options
/// under any other provider's name, silently, so they must go under the one
/// serving the request. A server whose listing names the model's providers
/// places them itself (and refuses what it cannot); for one that does not,
/// they go under every provider its endpoints name, and the request only to
/// those. Options already under a provider's name are sent as given.
async fn place_options(video: &VideoClient, body: &mut Map<String, Value>) -> Result<(), String> {
    let Some(Value::Object(options)) = body.get("provider").and_then(|p| p.get("options")).cloned()
    else {
        return Ok(());
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let listed = video
        .models()
        .await
        .map_err(|e| format!("{e:#}"))?
        .into_iter()
        .find(|m| m.id == model);
    let Some(listed) = listed else { return Ok(()) };
    if listed.providers.is_some() {
        return Ok(());
    }
    let (alone, named): (Map<String, Value>, Map<String, Value>) = options
        .into_iter()
        .partition(|(key, _)| listed.passthrough.contains(key));
    if alone.is_empty() {
        return Ok(());
    }
    let providers = video
        .providers_of(&model)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if providers.is_empty() {
        return Err(format!(
            "which providers serve {model} is not known: give its options under the provider's name in `provider_options`"
        ));
    }
    let mut placed = named;
    for slug in &providers {
        let own = match NESTED_UNDER.iter().find(|(name, _)| name == slug) {
            Some((_, under)) => json!({ *under: Value::Object(alone.clone()) }),
            None => Value::Object(alone.clone()),
        };
        placed.insert(slug.clone(), own);
    }
    let only: Vec<Value> = placed
        .keys()
        .filter(|k| *k != "brain")
        .cloned()
        .map(Value::String)
        .collect();
    body.insert(
        "provider".into(),
        json!({ "only": only, "options": Value::Object(placed) }),
    );
    Ok(())
}

/// Run the request `args` names (the arguments of `run_model`) against
/// `endpoint`, with paths relative to `workdir`, and write the clip.
/// `progress` hears each change of the job's state.
///
/// # Errors
/// The request is refused before anything is sent (a bad argument, a file
/// outside `workdir`), the provider refuses or fails it after the attempts,
/// or the clip is not ready within `config.timeout_secs`.
pub async fn generate(
    endpoint: &MediaEndpoint,
    config: &MediaConfig,
    workdir: &Path,
    args: &Value,
    progress: &mut (dyn FnMut(&str) + Send),
) -> Result<Generated, String> {
    let planned = plan(args, workdir)?;
    let (mut body, digests) = planned.with_media().await?;
    let video = client(endpoint).with_session(config.session.clone());
    place_options(&video, &mut body).await?;
    let dir = workdir.join(&config.dir);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(config.timeout_secs);
    for attempt in 0..MAX_ATTEMPTS {
        let key = request_key(&endpoint.base_url, &planned, &digests, attempt);
        let clip = dir.join(format!("{key}.mp4"));
        if clip.exists() {
            let sidecar = std::fs::read(sidecar_path(&clip))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Sidecar>(&raw).ok());
            let seconds = sidecar.as_ref().and_then(|s| s.seconds);
            let job_id = sidecar.as_ref().and_then(|s| s.job_id.clone());
            progress(&format!("the clip for request {key} is already made"));
            let path = save_as(&clip, planned.output.as_deref()).await?;
            return Ok(Generated {
                shown: shown(workdir, &path),
                path,
                reused: true,
                bytes: None,
                seconds,
                cost: None,
                file_id: sidecar.and_then(|s| s.file_id),
                job_id,
            });
        }
        let mut status = video
            .create(&Value::Object(body.clone()), &key)
            .await
            .map_err(|e| format!("{e:#}"))?;
        progress(&format!("request {key} is job {}", status.id));
        let mut last = String::new();
        while !status.state.finished() {
            if tokio::time::Instant::now() > deadline {
                let _ = video.cancel(&status.id).await;
                return Err(format!(
                    "the video was not ready within {} s; it was asked to stop",
                    config.timeout_secs
                ));
            }
            let now = describe(&status);
            if now != last {
                progress(&now);
                last = now;
            }
            tokio::time::sleep(POLL_EVERY).await;
            status = video
                .status(&status.id)
                .await
                .map_err(|e| format!("{e:#}"))?;
        }
        match status.state {
            VideoState::Completed => {
                progress("downloading the clip");
                let bytes = video
                    .download(&status.id, &clip)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                let sidecar = Sidecar {
                    model: planned.body["model"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    file_id: status.file_id.clone(),
                    seconds: status.seconds,
                    job_id: Some(status.id.clone()),
                };
                let _ = tokio::fs::write(
                    sidecar_path(&clip),
                    serde_json::to_vec_pretty(&sidecar).unwrap_or_default(),
                )
                .await;
                let path = save_as(&clip, planned.output.as_deref()).await?;
                return Ok(Generated {
                    shown: shown(workdir, &path),
                    path,
                    reused: false,
                    bytes: Some(bytes),
                    seconds: status.seconds,
                    cost: status.cost,
                    file_id: status.file_id.clone(),
                    job_id: Some(status.id.clone()),
                });
            }
            // A failed request is asked again under a fresh name: the same
            // name would only answer the same failure.
            VideoState::Failed(reason) if attempt + 1 < MAX_ATTEMPTS => {
                progress(&format!("the job failed ({reason}); asking again"));
            }
            VideoState::Failed(reason) => {
                return Err(format!("the video could not be made: {reason}"))
            }
            VideoState::Cancelled => return Err("the video was stopped before it was ready".into()),
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

/// A job's state as one progress line.
fn describe(status: &VideoStatus) -> String {
    match (&status.state, status.progress) {
        (_, Some((step, total))) => format!("job {} at step {step} of {total}", status.id),
        (VideoState::Pending, None) => format!("job {} is waiting", status.id),
        _ => format!("job {} is running", status.id),
    }
}

/// The clip also written at `output`, when asked (copied whole, then renamed
/// into place), and the path to give for it.
async fn save_as(clip: &Path, output: Option<&Path>) -> Result<PathBuf, String> {
    let Some(output) = output else {
        return Ok(clip.to_path_buf());
    };
    let partial = output.with_extension("mp4.part");
    tokio::fs::copy(clip, &partial)
        .await
        .and(tokio::fs::rename(&partial, output).await)
        .map_err(|e| format!("saving the video as {}: {e}", output.display()))?;
    Ok(output.to_path_buf())
}
