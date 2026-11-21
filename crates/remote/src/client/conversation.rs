// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The calls a host makes for one conversation: its shared document, the
//! files and messages a person adds to it, the approvals they decide, and
//! the events the host uploads.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sven_control::remote::{
    conversation_approvals_endpoint, conversation_doc_endpoint,
    conversation_file_complete_endpoint, conversation_files_endpoint,
    conversation_messages_endpoint, EventBatch,
};
use uuid::Uuid;

use super::errors::{superseded, Busy, ConversationNotFound, Disconnected, TokenRefused};
use super::{RemoteClient, REQUEST_TIMEOUT, UPLOAD_TIMEOUT};

impl RemoteClient {
    /// The update that brings a replica of conversation `id`'s shared
    /// document, whose state vector is `state_vector`, up to the server's.
    ///
    /// # Errors
    ///
    /// The token is refused, the conversation is not one that runs on this
    /// host ([`ConversationNotFound`]), or the server cannot be asked.
    pub async fn sync_conversation(
        &self,
        id: Uuid,
        state_vector: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        let response = self
            .http
            .get(self.url(&conversation_doc_endpoint(id)))
            .query(&[("sv", URL_SAFE_NO_PAD.encode(state_vector))])
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => return Err(TokenRefused.into()),
            404 => return Err(ConversationNotFound(id).into()),
            _ => {}
        }
        anyhow::ensure!(
            response.status().is_success(),
            "the conversation's document answered {}",
            response.status()
        );
        Ok(response.bytes().await?.to_vec())
    }

    /// Uploads the file at `path` to conversation `id`'s files, for the
    /// next message to carry; its id. The bytes go straight to the store the
    /// server names, which is sent no key.
    ///
    /// # Errors
    ///
    /// The file cannot be read, the token is refused, the conversation does
    /// not run on this host ([`ConversationNotFound`]), the server refuses the
    /// file (its reason), or the store does not take it.
    pub async fn upload_to_conversation(
        &self,
        id: Uuid,
        path: &std::path::Path,
    ) -> anyhow::Result<String> {
        use sha2::Digest as _;
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        let name = path
            .file_name()
            .map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
        let declared = self
            .http
            .post(self.url(&conversation_files_endpoint(id)))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({
                "name": name,
                "mimeType": media_type(path),
                "size": bytes.len(),
                "sha256": format!("{:x}", sha2::Sha256::digest(&bytes)),
            }))
            .send()
            .await?;
        let declared = refused_unless_ok(declared, id, "attaching the file").await?;
        let answer: serde_json::Value = declared.json().await?;
        let file_id = answer["file"]["uri"]
            .as_str()
            .and_then(|uri| uri.rsplit('/').next())
            .filter(|file_id| !file_id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("the server named no file for {name}"))?
            .to_string();
        let url = answer["upload"]["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the server named no place to upload {name}"))?;
        let mut put = self.http.put(url).timeout(UPLOAD_TIMEOUT).body(bytes);
        for (header, value) in answer["upload"]["headers"]
            .as_object()
            .into_iter()
            .flatten()
        {
            if let Some(value) = value.as_str() {
                put = put.header(header.as_str(), value);
            }
        }
        let stored = put.send().await?;
        anyhow::ensure!(
            stored.status().is_success(),
            "uploading {name} answered {}",
            stored.status()
        );
        let completed = self
            .http
            .post(self.url(&conversation_file_complete_endpoint(id, &file_id)))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({}))
            .send()
            .await?;
        refused_unless_ok(completed, id, "completing the upload").await?;
        Ok(file_id)
    }

    /// Adds `text` to conversation `id` as the person's next message, with
    /// the files `attachments` names (uploaded with
    /// [`Self::upload_to_conversation`]); the server then asks this host to
    /// answer it.
    ///
    /// # Errors
    ///
    /// The token is refused, the conversation does not run on this host
    /// ([`ConversationNotFound`]), it is answering already ([`Busy`]), the
    /// machine it runs on is not connected ([`Disconnected`]), or
    /// the server cannot be asked.
    pub async fn send_to_conversation(
        &self,
        id: Uuid,
        text: &str,
        attachments: &[String],
    ) -> anyhow::Result<()> {
        let mut body = serde_json::json!({ "text": text });
        // Named only when there are some: a server from before files takes
        // nothing but the text.
        if !attachments.is_empty() {
            body["attachments"] = serde_json::json!(attachments);
        }
        let response = self
            .http
            .post(self.url(&conversation_messages_endpoint(id)))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&body)
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => return Err(TokenRefused.into()),
            404 => return Err(ConversationNotFound(id).into()),
            409 => {
                let body = response.json::<serde_json::Value>().await.ok();
                let field = |name: &str| {
                    body.as_ref()
                        .and_then(|body| body.get(name)?.as_str().map(str::to_string))
                };
                if field("code").as_deref() == Some("environment_disconnected") {
                    let reason =
                        field("error").unwrap_or_else(|| "the machine is not connected".into());
                    return Err(Disconnected(reason).into());
                }
                let reason = field("error").unwrap_or_else(|| "the conversation is busy".into());
                return Err(Busy(reason).into());
            }
            _ => {}
        }
        anyhow::ensure!(
            response.status().is_success(),
            "sending the message answered {}",
            response.status()
        );
        Ok(())
    }

    /// Allows (`approve`) or refuses tool call `call_id` waiting in
    /// conversation `id`. False when nothing waits for that decision any
    /// more (it was decided elsewhere, or the answer ended).
    ///
    /// # Errors
    ///
    /// The token is refused, or the server cannot be asked.
    pub async fn decide_approval(
        &self,
        id: Uuid,
        call_id: &str,
        approve: bool,
    ) -> anyhow::Result<bool> {
        let response = self
            .http
            .post(self.url(&conversation_approvals_endpoint(id)))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "callId": call_id, "approve": approve }))
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => Err(TokenRefused.into()),
            404 => Ok(false),
            _ if response.status().is_success() => Ok(true),
            _ => anyhow::bail!("deciding the call answered {}", response.status()),
        }
    }

    /// Uploads `batch`.
    ///
    /// # Errors
    ///
    /// Not stored; the caller sends the same batch again.
    pub async fn post_events(&self, batch: &EventBatch) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering::Relaxed;
        let compressed = self.compress_events.load(Relaxed);
        let mut response = self.send_events(batch, compressed).await?;
        if compressed && response.status() == reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE {
            self.compress_events.store(false, Relaxed);
            response = self.send_events(batch, false).await?;
        } else if !compressed && sven_model_drivers::compression::takes_gzip(response.headers()) {
            self.compress_events.store(true, Relaxed);
        }
        if response.status().as_u16() == 409 {
            return Err(superseded(response).await.into());
        }
        anyhow::ensure!(
            response.status().is_success(),
            "the event upload answered {}",
            response.status()
        );
        Ok(())
    }

    /// Sends `batch`, gzip-compressed when `compressed`.
    async fn send_events(
        &self,
        batch: &EventBatch,
        compressed: bool,
    ) -> anyhow::Result<reqwest::Response> {
        let request = self
            .http
            .post(self.url("events"))
            .query(&self.environment_query())
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT);
        let request = if compressed {
            request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::CONTENT_ENCODING, "gzip")
                .body(sven_model_drivers::compression::gzip_json(batch)?)
        } else {
            request.json(batch)
        };
        Ok(request.send().await?)
    }
}

/// `response`, unless the server refused: the token ([`TokenRefused`]), the
/// conversation ([`ConversationNotFound`]), or `doing` for its own reason.
async fn refused_unless_ok(
    response: reqwest::Response,
    id: Uuid,
    doing: &str,
) -> anyhow::Result<reqwest::Response> {
    match response.status().as_u16() {
        200..=299 => Ok(response),
        401 | 403 => Err(TokenRefused.into()),
        404 => Err(ConversationNotFound(id).into()),
        status => {
            let reason = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|body| body.get("error")?.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("the server answered {status}"));
            Err(anyhow::anyhow!("{doing}: {reason}"))
        }
    }
}

/// The media type a file is uploaded as, from its extension: the picture
/// types a terminal attaches, anything else as bytes.
fn media_type(path: &std::path::Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        "md" => "text/markdown",
        "json" => "application/json",
        "txt" => "text/plain",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}
