// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The manifest written beside the Markdown file.
//!
//! It records, per page, how the text was obtained and the text itself, so a
//! rerun reuses accepted pages and an interrupted run loses at most the pages
//! in flight. It is rewritten atomically, so it is complete JSON at every
//! instant, including after a crash or a cancellation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::DocumentError;

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// Text taken from the PDF's own text layer.
    Native,
    /// Text produced by the OCR model.
    Ocr,
    /// A rendered page with no ink: nothing to read.
    Blank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Accepted; reused on resume.
    Ok,
    /// Output kept although a check failed; redone on resume.
    Warning,
    /// No usable output; redone on resume.
    Failed,
}

/// How a page was read when its layout was used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutInfo {
    /// Blocks the model located on the page.
    pub regions: usize,
    /// Text columns those blocks line up in.
    pub columns: usize,
    /// Blocks whose own crop could not be read cleanly, so the text of the
    /// layout pass stands in or a failed check was accepted.
    pub degraded: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRecord {
    /// 1-based page number.
    pub page: usize,
    /// Identity of the work: source, page, method, render config, model,
    /// prompt, token limit.
    pub key: String,
    pub method: Method,
    pub status: Status,
    /// Model spec for OCR pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub attempts: u32,
    /// Resolution of the accepted attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f32>,
    /// SHA-256 of `text`; a reused page must still match it.
    pub sha256: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub render_ms: u64,
    pub ocr_ms: u64,
    /// Present when the page was read block by block from its layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutInfo>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub source_sha256: String,
    pub page_count: usize,
    pub pages: Vec<PageRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Work identity of one page; every input that can change its text.
pub fn page_key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        // Length prefix keeps ("ab","c") distinct from ("a","bc").
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub fn manifest_path(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".manifest.json");
    destination.with_file_name(name)
}

impl Manifest {
    /// The manifest beside `destination`, if there is a readable one for this
    /// source. Anything else (missing, corrupt, another version or source) is
    /// "no previous run": correctness over hit rate.
    pub fn load_matching(destination: &Path, source_sha256: &str) -> Option<Self> {
        let bytes = std::fs::read(manifest_path(destination)).ok()?;
        let manifest: Self = serde_json::from_slice(&bytes).ok()?;
        (manifest.version == MANIFEST_VERSION && manifest.source_sha256 == source_sha256)
            .then_some(manifest)
    }

    /// A page of this manifest that may be reused for `key`.
    pub fn reusable(&self, page: usize, key: &str) -> Option<&PageRecord> {
        self.pages.iter().find(|p| {
            p.page == page
                && p.key == key
                && p.status == Status::Ok
                && p.sha256 == sha256_hex(p.text.as_bytes())
        })
    }

    pub fn write(&self, destination: &Path) -> Result<(), DocumentError> {
        let json = serde_json::to_vec_pretty(self).expect("manifest serialises");
        write_atomic(&manifest_path(destination), &json)
    }
}

/// Write `bytes` to a temporary file beside `path`, then rename over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), DocumentError> {
    let fail = |source| DocumentError::Write {
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(fail)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    let temp = path.with_file_name(name);
    std::fs::write(&temp, bytes).map_err(fail)?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        fail(e)
    })
}
