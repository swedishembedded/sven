// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for handing a person's files to
// the AI agent working for them for its clients. If your team needs
// expertise in agent file handling then you can procure our services by
// sending an email to info@swedishembedded.com.

//! Files a person attached to an input. The server holds them; the host
//! fetches each through the server's MCP endpoint (`files/authorizeDownload`,
//! checked against its digest) into the working directory, where the agent's
//! file tools reach it, and the input tells the agent where each one is. An
//! attached image the model takes is also given to it as an image in the
//! person's message, so it sees the picture without a tool to open the file
//! (the hosted agent has none).
//!
//! A machine runs many conversations, so every tool call and file request of
//! a session names its conversation in `_meta["sven/session"]`
//! ([`for_session`]): the server hands out only that conversation's files,
//! takes the agent's own uploads as that conversation's, and knows which
//! conversation a call is made in.

use std::path::Path;
use std::sync::Arc;

use sven_bootstrap::Config;
use sven_frontend::SessionController;
use sven_mcp_client::McpManager;
use sven_model::ContentPart;
use sven_tools_fs::AttachOptions;
use sven_vocab::ArtifactRef;
use uuid::Uuid;

use super::SessionActor;

/// Where attachments go, relative to the working directory; each file in a
/// directory named after its server-side id, so two files of one name never
/// overwrite each other and fetching one again rewrites it in place.
pub(super) const DIR: &str = ".sven/attachments";

/// `config` for session `id` working in `working_dir`: every MCP server
/// resolves the session's file arguments there, not in the host's own
/// directory, and the signed-in server's requests name the session.
pub(super) fn for_session(config: &Arc<Config>, id: Uuid, working_dir: &Path) -> Arc<Config> {
    if config.mcp_servers.is_empty() {
        return Arc::clone(config);
    }
    let mut own = Config::clone(config);
    for server in own.mcp_servers.values_mut() {
        server.file_root = Some(working_dir.to_path_buf());
    }
    if let Some(server) = own.mcp_servers.get_mut(sven_bootstrap::connectors::SERVER) {
        server.request_meta.insert(
            sven_bootstrap::connectors::SESSION_META.into(),
            serde_json::Value::String(id.to_string()),
        );
    }
    Arc::new(own)
}

/// The person's input with its attachments: the text, with every file listed
/// after it, and the images among them as the model's own input.
#[derive(Debug, Default)]
pub(super) struct Attached {
    pub(super) text: String,
    /// Empty when nothing attached is an image the model takes.
    pub(super) images: Vec<ContentPart>,
    /// Where each of `images` was saved, for a message the turn loads
    /// itself (a steering message).
    pub(super) image_paths: Vec<std::path::PathBuf>,
}

impl Attached {
    /// `next` said after this one: one message of both.
    pub(super) fn join(&mut self, next: Attached) {
        if !self.text.is_empty() && !next.text.is_empty() {
            self.text.push_str("\n\n");
        }
        self.text.push_str(&next.text);
        self.images.extend(next.images);
        self.image_paths.extend(next.image_paths);
    }
}

/// The image types the server's gateway takes as input
/// (`/v1/chat/completions` image parts).
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// `text` with `attachments` fetched into `working_dir` and listed after it.
/// A file that cannot be fetched is listed as such, so the agent can say so.
/// With `images` (what the conversation's model takes), an attached image it
/// takes is also loaded as an image part; one it does not take, or that will
/// not load, stays a file the agent's tools can reach.
pub(super) async fn attach(
    mcp: &McpManager,
    working_dir: &Path,
    text: String,
    attachments: &[ArtifactRef],
    images: Option<&AttachOptions>,
) -> Attached {
    if attachments.is_empty() {
        return Attached {
            text,
            ..Attached::default()
        };
    }
    let mut shown = Vec::new();
    let mut image_paths = Vec::new();
    let mut note = String::from("\n\n(Files I attached, saved in the working directory:");
    for file in attachments {
        let line = match fetch(mcp, working_dir, file).await {
            Ok(path) => {
                let parts = match images {
                    Some(opts) if IMAGE_TYPES.contains(&file.mime_type.as_str()) => {
                        image(&working_dir.join(&path), opts).await
                    }
                    _ => None,
                };
                let seen = if parts.is_some() {
                    image_paths.push(working_dir.join(&path));
                    "; it is also in this message as an image"
                } else {
                    ""
                };
                shown.extend(parts.unwrap_or_default());
                file.list_line(&path, seen)
            }
            Err(e) => {
                tracing::warn!("attachment {}: {e:#}", file.uri);
                format!("\n- {}: could not be fetched ({e:#})", file.name)
            }
        };
        note.push_str(&line);
    }
    note.push(')');
    Attached {
        text: text + &note,
        images: shown,
        image_paths,
    }
}

/// The image at `path` as the model's input, or `None` when the model does
/// not take images or the file is not one (it stays a file).
async fn image(path: &Path, opts: &AttachOptions) -> Option<Vec<ContentPart>> {
    match sven_tools_fs::load_attachment(path, opts, "the conversation's model").await {
        Ok(loaded @ sven_tools_fs::LoadedAttachment::Image { .. }) => {
            Some(loaded.into_content_parts())
        }
        Ok(_) => None,
        Err(e) => {
            tracing::debug!(
                "attachment {} is not shown as an image: {e}",
                path.display()
            );
            None
        }
    }
}

impl SessionActor {
    /// `text` with the files attached to it fetched and listed, and the
    /// images among them that the conversation's model takes; the live
    /// session is opened first, for its connection to the server.
    pub(super) async fn with_attachments(
        &mut self,
        text: String,
        attached: &[ArtifactRef],
    ) -> anyhow::Result<Attached> {
        if attached.is_empty() {
            return Ok(Attached {
                text,
                ..Attached::default()
            });
        }
        let model = self.model_config()?;
        if self.controller.is_none() {
            self.controller = Some(self.open_controller(model.clone()).await?);
        }
        let mcp = self.controller.as_ref().map(SessionController::mcp_manager);
        let mcp = mcp.expect("opened above");
        // What the model takes, as its server lists it: an image goes to a
        // model that sees images, and stays a file for one that does not.
        let opts = match sven_model_drivers::from_config_probed(&model).await {
            Ok(provider) => Some(AttachOptions::for_model(&*provider)),
            Err(e) => {
                tracing::warn!(
                    "session {}: the model's inputs are unknown, images stay files: {e:#}",
                    self.id
                );
                None
            }
        };
        Ok(attach(
            &mcp,
            &self.context.working_dir,
            text,
            attached,
            opts.as_ref(),
        )
        .await)
    }
}

/// Fetches `file`; its path relative to `working_dir`.
async fn fetch(mcp: &McpManager, working_dir: &Path, file: &ArtifactRef) -> anyhow::Result<String> {
    let id = file.uri.rsplit('/').next().unwrap_or_default();
    anyhow::ensure!(
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "not a file of the server: {}",
        file.uri
    );
    let dir = working_dir.join(DIR).join(id);
    let path = mcp
        .download_file(sven_bootstrap::connectors::SERVER, &file.uri, &dir)
        .await?;
    Ok(path
        .strip_prefix(working_dir)
        .unwrap_or(&path)
        .display()
        .to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use base64::Engine as _;
    use sha2::Digest as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    const BYTES: &[u8] = b"quarter,revenue\nQ3,12\n";

    /// A 1x1 PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// What the fake server holds as `mcp-file://files/f1`.
    #[derive(Clone, Copy)]
    struct Held {
        name: &'static str,
        mime: &'static str,
        bytes: &'static [u8],
    }

    const CSV: Held = Held {
        name: "q3.csv",
        mime: "text/csv",
        bytes: BYTES,
    };

    fn digest_of(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(bytes))
    }

    /// A server holding one file, `mcp-file://files/f1`, with `digest`: it
    /// answers `files/authorizeDownload` with a GET of its own, and keeps the
    /// `_meta` each request named.
    async fn server(digest: String, metas: Arc<Mutex<Vec<serde_json::Value>>>) -> String {
        server_of(CSV, digest, metas).await
    }

    async fn server_of(
        held: Held,
        digest: String,
        metas: Arc<Mutex<Vec<serde_json::Value>>>,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let blob = format!("{base}/blob");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (blob, digest, metas) = (blob.clone(), digest.clone(), Arc::clone(&metas));
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let end = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                    if head.starts_with("get /blob") {
                        let reply = format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            held.bytes.len()
                        );
                        let _ = socket.write_all(reply.as_bytes()).await;
                        let _ = socket.write_all(held.bytes).await;
                        return;
                    }
                    let length: usize = head
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.trim() == "content-length")
                        .and_then(|(_, v)| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < end + length {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let request: serde_json::Value =
                        serde_json::from_slice(&buf[end..]).unwrap_or_default();
                    let result = match request["method"].as_str().unwrap_or_default() {
                        "initialize" => Some(serde_json::json!({
                            "protocolVersion": "2025-11-25",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "files", "version": "1" },
                        })),
                        "tools/list" => Some(serde_json::json!({ "tools": [] })),
                        "files/authorizeDownload"
                            if request["params"]["uri"] == "mcp-file://files/f1" =>
                        {
                            metas
                                .lock()
                                .unwrap()
                                .push(request["params"]["_meta"].clone());
                            Some(serde_json::json!({
                                "file": { "uri": "mcp-file://files/f1", "name": held.name, "mimeType": held.mime,
                                          "size": held.bytes.len(), "digest": { "algorithm": "sha-256", "value": digest } },
                                "download": { "transport": "https", "method": "GET", "url": blob },
                            }))
                        }
                        _ => None,
                    };
                    let reply = match (request.get("id"), result) {
                        (Some(id), Some(result)) => {
                            let body = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                            format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                        }
                        (Some(id), None) => {
                            let body = serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "no such file" } }).to_string();
                            format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                        }
                        _ => "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                    };
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });
        format!("{base}/mcp")
    }

    fn attachment(uri: &str, name: &str) -> ArtifactRef {
        ArtifactRef {
            uri: uri.into(),
            name: name.into(),
            mime_type: "text/csv".into(),
            size: BYTES.len() as u64,
        }
    }

    /// Connected as session `id` of a host signed in at `url`.
    async fn manager(url: &str, id: Uuid, home: &Path) -> Arc<McpManager> {
        let mut config = Config::default();
        sven_bootstrap::connectors::add(&mut config, url, None, "key");
        let own = for_session(&Arc::new(config), id, home);
        let (events, _) = tokio::sync::mpsc::channel(64);
        let manager = McpManager::with_token_store(
            own.mcp_servers.clone(),
            events,
            false,
            sven_mcp_client::TokenStore::new(home.join("t.json")),
        );
        manager
            .connect(sven_bootstrap::connectors::SERVER)
            .await
            .unwrap();
        manager
    }

    /// An attached file lands in the working directory under its id, the
    /// input names where, the request named the session; a file the server
    /// refuses, or whose bytes do not match, is named as not fetched and
    /// nothing of it is kept.
    #[tokio::test(flavor = "multi_thread")]
    async fn attached_files_are_fetched_into_the_working_directory_and_listed() {
        let digest =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(BYTES));
        let metas = Arc::new(Mutex::new(Vec::new()));
        let url = server(digest, Arc::clone(&metas)).await;
        let work = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mcp = manager(&url, id, work.path()).await;

        let attached = attach(
            &mcp,
            work.path(),
            "Sum the revenue.".into(),
            &[
                attachment("mcp-file://files/f1", "q3.csv"),
                attachment("mcp-file://files/gone", "q4.csv"),
            ],
            Some(&AttachOptions {
                supports_images: true,
                ..AttachOptions::default()
            }),
        )
        .await;
        // A file that is no image is no image part, whatever the model takes.
        assert!(attached.images.is_empty());
        let text = attached.text;
        let kept = work.path().join(DIR).join("f1/q3.csv");
        assert_eq!(std::fs::read(&kept).unwrap(), BYTES);
        assert!(text.starts_with("Sum the revenue."));
        assert!(
            text.contains("`.sven/attachments/f1/q3.csv` (text/csv, 22 bytes)"),
            "{text}"
        );
        assert!(text.contains("q4.csv: could not be fetched"), "{text}");
        assert_eq!(
            metas.lock().unwrap()[0][sven_bootstrap::connectors::SESSION_META],
            serde_json::json!(id.to_string())
        );

        let tampered = server("A".repeat(43), Arc::new(Mutex::new(Vec::new()))).await;
        let elsewhere = tempfile::tempdir().unwrap();
        let mcp = manager(&tampered, id, elsewhere.path()).await;
        let text = attach(
            &mcp,
            elsewhere.path(),
            String::new(),
            &[attachment("mcp-file://files/f1", "q3.csv")],
            None,
        )
        .await
        .text;
        assert!(text.contains("did not match its digest"), "{text}");
        assert!(!elsewhere.path().join(DIR).join("f1/q3.csv").exists());
    }

    /// An attached image goes to a model that takes images as an image in the
    /// person's message, so it is seen without a tool to open the file; for a
    /// model that does not, it is only the file. Either way the file is kept
    /// and listed, for the agent's tools.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_attached_image_is_the_models_own_input_when_it_takes_images() {
        let png = Held {
            name: "shot.png",
            mime: "image/png",
            bytes: PNG,
        };
        let url = server_of(png, digest_of(PNG), Arc::new(Mutex::new(Vec::new()))).await;
        let image = ArtifactRef {
            uri: "mcp-file://files/f1".into(),
            name: "shot.png".into(),
            mime_type: "image/png".into(),
            size: PNG.len() as u64,
        };

        let work = tempfile::tempdir().unwrap();
        let mcp = manager(&url, Uuid::new_v4(), work.path()).await;
        let sees = AttachOptions {
            supports_images: true,
            ..AttachOptions::default()
        };
        let attached = attach(
            &mcp,
            work.path(),
            "What do you see?".into(),
            std::slice::from_ref(&image),
            Some(&sees),
        )
        .await;
        assert!(
            attached.text.starts_with("What do you see?"),
            "{}",
            attached.text
        );
        assert!(
            attached.text.contains("`.sven/attachments/f1/shot.png`"),
            "{}",
            attached.text
        );
        assert!(
            attached.text.contains("also in this message as an image"),
            "{}",
            attached.text
        );
        let urls: Vec<_> = attached
            .images
            .iter()
            .filter_map(|part| match part {
                ContentPart::Image { image_url, .. } => Some(image_url.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("data:image/png;base64,"), "{}", urls[0]);
        assert!(work.path().join(DIR).join("f1/shot.png").exists());

        let blind = tempfile::tempdir().unwrap();
        let mcp = manager(&url, Uuid::new_v4(), blind.path()).await;
        let text_only = AttachOptions {
            supports_images: false,
            ..AttachOptions::default()
        };
        let attached = attach(
            &mcp,
            blind.path(),
            "What do you see?".into(),
            &[image],
            Some(&text_only),
        )
        .await;
        assert!(attached.images.is_empty());
        assert!(
            attached.text.contains("`.sven/attachments/f1/shot.png`"),
            "{}",
            attached.text
        );
        assert!(!attached.text.contains("as an image"), "{}", attached.text);
    }

    /// Only the signed-in server's file requests name the session; a host
    /// without one shares its configuration untouched.
    #[test]
    fn a_host_without_a_server_shares_its_configuration() {
        let plain = Arc::new(Config::default());
        assert!(Arc::ptr_eq(
            &for_session(&plain, Uuid::new_v4(), Path::new("/work")),
            &plain
        ));
    }
}
