// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Files over MCP: the client side of MCP's file transfer extension
//! (SEP-2631, PR revision of 2026-06-16; now the experimental extension
//! `experimental-ext-file-uploads`), for any server that offers it.
//!
//! A tool marks a URI string argument with `x-mcp-file`. When the model
//! writes a local path there, sven asks the server `files/authorizeUpload`
//! (name, media type, size, SHA-256), uploads the bytes to the URL it is
//! given and sends the file URI the server minted instead of the path. A
//! file URI the server hands back is fetched with `files/authorizeDownload`
//! and checked against its digest. The model never sees a transfer URL, and
//! a path outside the directory sven works in is refused.
//!
//! Kept in one module because the extension is experimental and can change.
//!
//! Swedish Embedded AB implements solutions for giving AI agents safe access
//! to files across services for its clients. If your team needs expertise
//! in MCP integrations then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use rmcp::model::{ClientCapabilities, JsonObject};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::connection::Connection;

/// The JSON Schema keyword that marks a file-valued argument.
pub const FILE_KEYWORD: &str = "x-mcp-file";
/// The extension's identifier among a client's capabilities.
pub const EXTENSION: &str = "io.modelcontextprotocol/files";
/// The largest file sven uploads in one request.
pub const MAX_UPLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// The capabilities sven declares: the file transfer extension, both ways,
/// over HTTPS.
#[must_use]
pub fn capabilities() -> ClientCapabilities {
    let mut settings = JsonObject::new();
    settings.insert("upload".into(), Value::Bool(true));
    settings.insert("download".into(), Value::Bool(true));
    settings.insert("transports".into(), json!(["https"]));
    let mut capabilities = ClientCapabilities::default();
    capabilities.extensions = Some([(EXTENSION.to_string(), settings)].into_iter().collect());
    capabilities
}

/// The top-level arguments of a tool's input schema that take files, and
/// whether each takes a list of them.
#[must_use]
pub fn file_fields(schema: &Value) -> Vec<(String, bool)> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .filter_map(|(name, property)| {
            if property.get(FILE_KEYWORD).is_some() {
                Some((name.clone(), false))
            } else if property
                .get("items")
                .is_some_and(|items| items.get(FILE_KEYWORD).is_some())
            {
                Some((name.clone(), true))
            } else {
                None
            }
        })
        .collect()
}

/// Where local paths resolve, and what a host adds to every file request's
/// `_meta` (the session a server should file an upload under).
#[derive(Debug, Clone, Default)]
pub struct FileContext {
    pub root: PathBuf,
    pub meta: Map<String, Value>,
}

/// Whether `value` already names a file rather than a local path.
fn names_a_file(value: &str) -> bool {
    value.contains("://") || value.starts_with("data:")
}

/// Replaces each local path the model wrote in a file argument with the file
/// URI the server minted for it, uploading it first.
pub async fn prepare_arguments(
    connection: &Connection,
    schema: &Value,
    args: &Value,
    context: &FileContext,
) -> Result<Value> {
    let fields = file_fields(schema);
    if fields.is_empty() {
        return Ok(args.clone());
    }
    let mut args = args.clone();
    let Some(object) = args.as_object_mut() else {
        return Ok(args);
    };
    for (name, list) in fields {
        match object.get_mut(&name) {
            Some(Value::String(path)) if !list && !names_a_file(path) => {
                *path = upload(connection, context, path).await?;
            }
            Some(Value::Array(items)) if list => {
                for item in items.iter_mut() {
                    if let Value::String(path) = item {
                        if !names_a_file(path) {
                            *path = upload(connection, context, path).await?;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(args)
}

/// The local file `path` names, inside `root` and nowhere else.
fn confined(root: &Path, path: &str) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot read {}", root.display()))?;
    let real = root
        .join(path)
        .canonicalize()
        .with_context(|| format!("{path}: no such file"))?;
    if !real.starts_with(&root) {
        bail!("{path} is outside the directory sven works in");
    }
    if !real.is_file() {
        bail!("{path} is not a file");
    }
    Ok(real)
}

/// Uploads the file at `path` (relative to the context's root) and answers
/// the URI the server minted for it.
pub async fn upload(connection: &Connection, context: &FileContext, path: &str) -> Result<String> {
    let real = confined(&context.root, path)?;
    let size = std::fs::metadata(&real)?.len();
    if size > MAX_UPLOAD_BYTES {
        bail!("{path} is larger than the {MAX_UPLOAD_BYTES} bytes sven uploads");
    }
    let bytes = tokio::fs::read(&real)
        .await
        .with_context(|| format!("reading {path}"))?;
    let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes));
    let name = real
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let mut params = json!({
        "name": name,
        "mimeType": media_type(&real),
        "size": size,
        "digest": { "algorithm": "sha-256", "value": digest },
    });
    if !context.meta.is_empty() {
        params["_meta"] = Value::Object(context.meta.clone());
    }
    let answer = connection
        .request("files/authorizeUpload", params)
        .await
        .context("files/authorizeUpload")?;
    let uri = answer
        .pointer("/file/uri")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the server answered no file URI"))?
        .to_string();
    let upload = answer
        .get("upload")
        .ok_or_else(|| anyhow!("the server answered no upload"))?;
    let url = upload.get("url").and_then(Value::as_str).ok_or_else(|| {
        anyhow!("the server asked for an upload sven does not make (only a single HTTPS request)")
    })?;
    let method = upload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("PUT");
    if upload.get("multipart").is_some() || !matches!(method, "PUT" | "POST") {
        bail!("the server asked for a {method} form upload, which sven does not make");
    }
    let mut request = sven_http::client().request(
        reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::PUT),
        url,
    );
    if let Some(headers) = upload.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            // The client sends the body's length itself.
            if name.eq_ignore_ascii_case("content-length") {
                continue;
            }
            if let Some(value) = value.as_str() {
                request = request.header(name, value);
            }
        }
    }
    let response = request.body(bytes).send().await.context("uploading")?;
    if !response.status().is_success() {
        bail!("the upload of {path} was refused ({})", response.status());
    }
    Ok(uri)
}

/// Fetches the file `uri` names into `dir`, checks it against its digest,
/// and answers where it is.
pub async fn download(
    connection: &Connection,
    context: &FileContext,
    uri: &str,
    dir: &Path,
) -> Result<PathBuf> {
    let mut params = json!({ "uri": uri });
    if !context.meta.is_empty() {
        params["_meta"] = Value::Object(context.meta.clone());
    }
    let answer = connection
        .request("files/authorizeDownload", params)
        .await
        .context("files/authorizeDownload")?;
    let url = answer
        .pointer("/download/url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the server answered no download"))?;
    let name = answer
        .pointer("/file/name")
        .and_then(Value::as_str)
        .map(safe_name)
        .unwrap_or_else(|| "file".to_string());
    let expected = answer
        .pointer("/file/digest/value")
        .and_then(Value::as_str)
        .map(str::to_string);
    let response = sven_http::client()
        .get(url)
        .send()
        .await
        .context("downloading")?;
    if !response.status().is_success() {
        bail!("the download of {uri} was refused ({})", response.status());
    }
    let bytes = response.bytes().await.context("downloading")?;
    if let Some(expected) = expected {
        let got = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes));
        if got != expected.trim_end_matches('=') {
            bail!("{uri} did not match its digest; nothing was kept");
        }
    }
    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(name);
    tokio::fs::write(&path, &bytes).await?;
    Ok(path)
}

/// A file name that stays one path segment.
fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = last.chars().filter(|c| !c.is_control()).collect();
    match clean.trim() {
        "" | "." | ".." => "file".to_string(),
        name => name.to_string(),
    }
}

/// A media type from a file's extension; `application/octet-stream` when
/// it is not one sven knows.
#[must_use]
pub fn media_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "step" | "stp" => "model/step",
        "stl" => "model/stl",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_fields_are_the_marked_ones_and_lists_of_them() {
        let schema = json!({
            "type": "object",
            "properties": {
                "drawing": { "type": "string", "format": "uri", "x-mcp-file": { "accept": ["image/*"] } },
                "sheets": { "type": "array", "items": { "type": "string", "x-mcp-file": {} } },
                "title": { "type": "string" }
            }
        });
        let mut fields = file_fields(&schema);
        fields.sort();
        assert_eq!(
            fields,
            vec![("drawing".to_string(), false), ("sheets".to_string(), true)]
        );
        assert!(file_fields(&json!({"type": "object"})).is_empty());
    }

    #[test]
    fn a_path_must_stay_in_the_directory_sven_works_in() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "a").unwrap();
        assert!(confined(root.path(), "a.txt").is_ok());
        assert!(confined(root.path(), "../etc/passwd").is_err());
        assert!(confined(root.path(), "/etc/passwd").is_err());
        assert!(confined(root.path(), "missing.txt").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", root.path().join("out")).unwrap();
            assert!(
                confined(root.path(), "out").is_err(),
                "a symlink out is refused"
            );
        }
    }

    #[test]
    fn names_stay_one_segment_and_types_follow_extensions() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(
            media_type(Path::new("Budget.XLSX")),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(media_type(Path::new("part")), "application/octet-stream");
    }

    #[test]
    fn sven_declares_the_extension() {
        let declared = capabilities();
        let settings = &declared.extensions.unwrap()[EXTENSION];
        assert_eq!(settings["upload"], true);
        assert_eq!(settings["download"], true);
    }
}
