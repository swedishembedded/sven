// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting generative video
// behind agent tools for its clients. If your team needs expertise in
// media-generation APIs then you can procure our services by sending an email
// to info@swedishembedded.com.
//! Video generation from an OpenRouter-compatible server: submit a job to
//! `POST {base}/videos`, poll `GET {base}/videos/{id}`, download
//! `GET {base}/videos/{id}/content`, list `GET {base}/videos/models`. The
//! same client speaks to a local brain, OpenRouter and the sven service.
//!
//! Every answer is checked: a status is one of the known words, a job id is
//! one this client will put in a path, and a download is a video media type
//! written to the file it was asked for, nowhere else.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};

/// How long one request other than a download may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The largest clip written to disk.
const MAX_VIDEO_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Where a job is.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoState {
    Pending,
    InProgress,
    Completed,
    Failed(String),
    Cancelled,
    Expired,
}

impl VideoState {
    /// Whether the job has stopped changing.
    #[must_use]
    pub fn finished(&self) -> bool {
        !matches!(self, Self::Pending | Self::InProgress)
    }
}

/// One poll of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoStatus {
    pub id: String,
    pub state: VideoState,
    /// `(step, total)` while it renders, when the server says.
    pub progress: Option<(u64, u64)>,
    /// Seconds delivered, once completed, when the server says.
    pub seconds: Option<f64>,
    /// What it cost, in dollars, when the server says.
    pub cost: Option<f64>,
    /// The server's own stored file of the result, when it keeps one.
    pub file_id: Option<String>,
}

/// One video model a server lists.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoModel {
    pub id: String,
    pub name: Option<String>,
    pub durations: Vec<u64>,
    pub resolutions: Vec<String>,
    pub aspect_ratios: Vec<String>,
    pub generate_audio: Option<bool>,
    pub seed: bool,
    /// Edits the model offers beyond generation (`extend`, `upscale`, ...).
    pub edits: Vec<String>,
    /// The other actions it offers besides plain generation of the prompt
    /// alone, e.g. `ref2va` (generation from references).
    pub actions: Vec<String>,
    /// What it takes as references (`image`, `video`, `audio`); empty when the
    /// listing does not say.
    pub supported_reference_types: Vec<String>,
    /// Its price per second, by the listing's own key, as listed.
    pub pricing: Vec<(String, String)>,
}

/// A server's video API, at its OpenAI-style base URL (ending in `/v1`).
#[derive(Debug, Clone)]
pub struct VideoClient {
    base_url: String,
    api_key: String,
}

fn job_id(value: &Value) -> anyhow::Result<String> {
    let id = value["id"]
        .as_str()
        .ok_or_else(|| anyhow!("the video answer has no id"))?;
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !ok {
        bail!(
            "the server named a video job whose id cannot be used: {}",
            id.chars().take(40).collect::<String>()
        );
    }
    Ok(id.to_owned())
}

/// A status answer, read field by field.
fn parse_status(value: &Value) -> anyhow::Result<VideoStatus> {
    let state = match value["status"].as_str() {
        Some("pending") => VideoState::Pending,
        Some("in_progress") => VideoState::InProgress,
        Some("completed") => VideoState::Completed,
        Some("failed") => VideoState::Failed(
            value["error"]
                .as_str()
                .unwrap_or("the video could not be made")
                .chars()
                .take(300)
                .collect(),
        ),
        Some("cancelled") => VideoState::Cancelled,
        Some("expired") => VideoState::Expired,
        other => bail!("the server reported a video status this client does not know: {other:?}"),
    };
    let progress = match (
        value["progress"]["step"].as_u64(),
        value["progress"]["total"].as_u64(),
    ) {
        (Some(step), Some(total)) if total > 0 => Some((step.min(total), total)),
        _ => None,
    };
    Ok(VideoStatus {
        id: job_id(value)?,
        state,
        progress,
        seconds: value["usage"]["seconds"]
            .as_f64()
            .filter(|s| s.is_finite() && *s >= 0.0),
        cost: value["usage"]["cost"]
            .as_f64()
            .filter(|c| c.is_finite() && *c >= 0.0),
        file_id: value["file_id"].as_str().map(str::to_owned),
    })
}

fn parse_models(value: &Value) -> Vec<VideoModel> {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    value["data"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| {
                    let id = m["id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 200)?;
                    let mut pricing: Vec<(String, String)> = m["pricing_skus"]
                        .as_object()
                        .map(|o| {
                            o.iter()
                                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                                .collect()
                        })
                        .unwrap_or_default();
                    pricing.sort();
                    Some(VideoModel {
                        id: id.to_owned(),
                        name: m["name"].as_str().map(str::to_owned),
                        durations: m["supported_durations"]
                            .as_array()
                            .map(|a| a.iter().filter_map(Value::as_u64).collect())
                            .unwrap_or_default(),
                        resolutions: strings(&m["supported_resolutions"]),
                        aspect_ratios: strings(&m["supported_aspect_ratios"]),
                        generate_audio: m["generate_audio"].as_bool(),
                        seed: m["seed"].as_bool().unwrap_or(false),
                        supported_reference_types: strings(&m["supported_reference_types"]),
                        actions: m["actions"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter(|x| x["edits"].as_bool() != Some(true))
                                    .filter_map(|x| x["action"].as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        edits: m["actions"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter(|x| x["edits"].as_bool() == Some(true))
                                    .filter_map(|x| x["action"].as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        pricing,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl VideoClient {
    #[must_use]
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    async fn read(response: reqwest::Response, what: &str) -> anyhow::Result<Value> {
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("{what}: reading the answer"))?;
        let json: Option<Value> = serde_json::from_str(&body).ok();
        if !status.is_success() {
            let message = json
                .as_ref()
                .and_then(|j| {
                    j["error"]["message"]
                        .as_str()
                        .or_else(|| j["error"].as_str())
                })
                .map_or_else(|| body.chars().take(200).collect(), str::to_owned);
            bail!("{what}: HTTP {}: {message}", status.as_u16());
        }
        json.ok_or_else(|| anyhow!("{what}: the answer is not JSON"))
    }

    /// Start a job. `idempotency_key` names it: the same key again answers the
    /// same job instead of making another.
    ///
    /// # Errors
    /// The server cannot be reached, refuses the request (its message is
    /// kept), or answers without a usable id.
    pub async fn create(
        &self,
        request: &Value,
        idempotency_key: &str,
    ) -> anyhow::Result<VideoStatus> {
        let response = crate::build_http_client()
            .post(format!("{}/videos", self.base_url))
            .bearer_auth(&self.api_key)
            .header("idempotency-key", idempotency_key)
            .timeout(REQUEST_TIMEOUT)
            .json(request)
            .send()
            .await
            .context("starting the video")?;
        let answer = Self::read(response, "starting the video").await?;
        parse_status(&answer).or_else(|_| {
            Ok(VideoStatus {
                id: job_id(&answer)?,
                state: VideoState::Pending,
                progress: None,
                seconds: None,
                cost: None,
                file_id: None,
            })
        })
    }

    /// Where job `id` is.
    ///
    /// # Errors
    /// As [`Self::create`].
    pub async fn status(&self, id: &str) -> anyhow::Result<VideoStatus> {
        let id = job_id(&json!({ "id": id }))?;
        let response = crate::build_http_client()
            .get(format!("{}/videos/{id}", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("asking about the video")?;
        parse_status(&Self::read(response, "asking about the video").await?)
    }

    /// Ask job `id` to stop (or a finished one to be deleted). False when the
    /// server has no way to.
    ///
    /// # Errors
    /// The server cannot be reached or answers an error other than "no".
    pub async fn cancel(&self, id: &str) -> anyhow::Result<bool> {
        let id = job_id(&json!({ "id": id }))?;
        let response = crate::build_http_client()
            .delete(format!("{}/videos/{id}", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("stopping the video")?;
        if matches!(response.status().as_u16(), 404 | 405) {
            return Ok(false);
        }
        Self::read(response, "stopping the video")
            .await
            .map(|_| true)
    }

    /// Download finished job `id` to `path` (written whole, then renamed into
    /// place, so a failed download leaves no partial file there). Answers the
    /// bytes written.
    ///
    /// # Errors
    /// The server cannot be reached, refuses, sends something that is not a
    /// video, sends more than the cap, or the file cannot be written.
    pub async fn download(&self, id: &str, path: &Path) -> anyhow::Result<u64> {
        use tokio::io::AsyncWriteExt;
        let id = job_id(&json!({ "id": id }))?;
        let mut response = crate::build_http_client()
            .get(format!("{}/videos/{id}/content?index=0", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .context("downloading the video")?;
        if !response.status().is_success() {
            Self::read(response, "downloading the video").await?;
            bail!("downloading the video: refused");
        }
        let media = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
        if !media.starts_with("video/") {
            bail!("downloading the video: the server sent {media:?}, not a video");
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let partial = path.with_extension("part");
        let mut file = tokio::fs::File::create(&partial)
            .await
            .with_context(|| format!("writing {}", partial.display()))?;
        let mut written = 0u64;
        while let Some(chunk) = response.chunk().await.context("downloading the video")? {
            written += chunk.len() as u64;
            if written > MAX_VIDEO_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&partial).await;
                bail!("downloading the video: larger than {MAX_VIDEO_BYTES} bytes");
            }
            file.write_all(&chunk)
                .await
                .with_context(|| format!("writing {}", partial.display()))?;
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&partial, path)
            .await
            .with_context(|| format!("moving the video to {}", path.display()))?;
        Ok(written)
    }

    /// The server's video models; empty when it has none.
    ///
    /// # Errors
    /// The server cannot be reached or answers an error other than "no such
    /// route".
    pub async fn models(&self) -> anyhow::Result<Vec<VideoModel>> {
        let response = crate::build_http_client()
            .get(format!("{}/videos/models", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("listing video models")?;
        if response.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        Ok(parse_models(
            &Self::read(response, "listing video models").await?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_is_read_field_by_field_and_an_unknown_word_refused() {
        let done = parse_status(&json!({"id": "vid_1", "status": "completed", "usage": {"seconds": 5.0, "cost": 0.5}, "file_id": "f1"})).unwrap();
        assert_eq!(
            (done.state, done.seconds, done.cost, done.file_id.as_deref()),
            (VideoState::Completed, Some(5.0), Some(0.5), Some("f1"))
        );
        let running = parse_status(
            &json!({"id": "vid_1", "status": "in_progress", "progress": {"step": 9, "total": 4}}),
        )
        .unwrap();
        assert_eq!(running.progress, Some((4, 4)));
        assert!(parse_status(&json!({"id": "vid_1", "status": "rendering"})).is_err());
        assert!(parse_status(&json!({"id": "../etc", "status": "pending"})).is_err());
        assert_eq!(
            parse_status(&json!({"id": "v", "status": "failed"}))
                .unwrap()
                .state,
            VideoState::Failed("the video could not be made".into())
        );
    }

    #[test]
    fn models_are_read_with_their_edits_and_prices() {
        let models = parse_models(&json!({"data": [
            {"id": "brain/mock-video", "supported_durations": [1, 2], "supported_resolutions": ["480p"], "seed": true,
             "supported_reference_types": ["image", "audio"],
             "actions": [{"action": "t2v", "edits": false}, {"action": "extend", "edits": true}],
             "pricing_skus": {"duration_seconds": "0.01"}},
            {"id": ""},
        ]}));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].edits, ["extend"]);
        assert_eq!(models[0].actions, ["t2v"]);
        assert_eq!(models[0].supported_reference_types, ["image", "audio"]);
        assert_eq!(models[0].durations, [1, 2]);
        assert_eq!(
            models[0].pricing,
            [("duration_seconds".to_owned(), "0.01".to_owned())]
        );
    }
}
