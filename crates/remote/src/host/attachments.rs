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
//! file tools reach it, and the input tells the agent where each one is.
//!
//! A machine runs many conversations, so every file request of a session
//! names its conversation in `_meta["sven/session"]` ([`for_session`]): the
//! server hands out only that conversation's files, and takes the agent's
//! own uploads as that conversation's.

use std::path::Path;
use std::sync::Arc;

use sven_bootstrap::Config;
use sven_control::Attachment;
use sven_frontend::SessionController;
use sven_mcp_client::McpManager;
use uuid::Uuid;

use super::SessionActor;

/// Where attachments go, relative to the working directory; each file in a
/// directory named after its server-side id, so two files of one name never
/// overwrite each other and fetching one again rewrites it in place.
pub(super) const DIR: &str = ".sven/attachments";

/// The `_meta` key that names a session's conversation in file requests.
const SESSION_META: &str = "sven/session";

/// `config` for session `id`: its server's file requests name the session.
pub(super) fn for_session(config: &Arc<Config>, id: Uuid) -> Arc<Config> {
    let server = sven_bootstrap::connectors::SERVER;
    if !config.mcp_servers.contains_key(server) {
        return Arc::clone(config);
    }
    let mut own = Config::clone(config);
    if let Some(server) = own.mcp_servers.get_mut(server) {
        server.file_meta.insert(
            SESSION_META.into(),
            serde_json::Value::String(id.to_string()),
        );
    }
    Arc::new(own)
}

/// `text` with `attachments` fetched into `working_dir` and listed after it.
/// A file that cannot be fetched is listed as such, so the agent can say so.
pub(super) async fn attach(
    mcp: &McpManager,
    working_dir: &Path,
    text: String,
    attachments: &[Attachment],
) -> String {
    if attachments.is_empty() {
        return text;
    }
    let mut note = String::from("\n\n(Files I attached, saved in the working directory:");
    for file in attachments {
        let line = match fetch(mcp, working_dir, file).await {
            Ok(path) => format!("\n- `{path}` ({}, {} bytes)", file.mime_type, file.size),
            Err(e) => {
                tracing::warn!("attachment {}: {e:#}", file.uri);
                format!("\n- {}: could not be fetched ({e:#})", file.name)
            }
        };
        note.push_str(&line);
    }
    note.push(')');
    text + &note
}

impl SessionActor {
    /// `text` with the files attached to it fetched and listed; the live
    /// session is opened first, for its connection to the server.
    pub(super) async fn with_attachments(
        &mut self,
        text: String,
        attached: &[Attachment],
    ) -> anyhow::Result<String> {
        if attached.is_empty() {
            return Ok(text);
        }
        if self.controller.is_none() {
            let model = self.model_config()?;
            self.controller = Some(self.open_controller(model).await?);
        }
        let mcp = self.controller.as_ref().map(SessionController::mcp_manager);
        let mcp = mcp.expect("opened above");
        Ok(attach(&mcp, &self.context.working_dir, text, attached).await)
    }
}

/// Fetches `file`; its path relative to `working_dir`.
async fn fetch(mcp: &McpManager, working_dir: &Path, file: &Attachment) -> anyhow::Result<String> {
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

    /// A server holding one file, `mcp-file://files/f1`, with `digest`: it
    /// answers `files/authorizeDownload` with a GET of its own, and keeps the
    /// `_meta` each request named.
    async fn server(digest: String, metas: Arc<Mutex<Vec<serde_json::Value>>>) -> String {
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
                            BYTES.len()
                        );
                        let _ = socket.write_all(reply.as_bytes()).await;
                        let _ = socket.write_all(BYTES).await;
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
                                "file": { "uri": "mcp-file://files/f1", "name": "q3.csv", "mimeType": "text/csv",
                                          "size": BYTES.len(), "digest": { "algorithm": "sha-256", "value": digest } },
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

    fn attachment(uri: &str, name: &str) -> Attachment {
        Attachment {
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
        let own = for_session(&Arc::new(config), id);
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

        let text = attach(
            &mcp,
            work.path(),
            "Sum the revenue.".into(),
            &[
                attachment("mcp-file://files/f1", "q3.csv"),
                attachment("mcp-file://files/gone", "q4.csv"),
            ],
        )
        .await;
        let kept = work.path().join(DIR).join("f1/q3.csv");
        assert_eq!(std::fs::read(&kept).unwrap(), BYTES);
        assert!(text.starts_with("Sum the revenue."));
        assert!(
            text.contains("`.sven/attachments/f1/q3.csv` (text/csv, 22 bytes)"),
            "{text}"
        );
        assert!(text.contains("q4.csv: could not be fetched"), "{text}");
        assert_eq!(
            metas.lock().unwrap()[0][SESSION_META],
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
        )
        .await;
        assert!(text.contains("did not match its digest"), "{text}");
        assert!(!elsewhere.path().join(DIR).join("f1/q3.csv").exists());
    }

    /// Only the signed-in server's file requests name the session; a host
    /// without one shares its configuration untouched.
    #[test]
    fn a_host_without_a_server_shares_its_configuration() {
        let plain = Arc::new(Config::default());
        assert!(Arc::ptr_eq(&for_session(&plain, Uuid::new_v4()), &plain));
    }
}
