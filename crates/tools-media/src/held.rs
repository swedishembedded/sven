// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that hand a person's
// files to the models they run without moving them twice for its clients.
// If your team needs expertise in agent file handling then you can procure
// our services by sending an email to info@swedishembedded.com.

//! A working-directory copy of a file the service already holds (an
//! attachment the person gave): which file it is, and what its bytes were
//! when fetched. A request that reads it names the service's file instead of
//! sending the bytes (a large image goes up once, by reference, however
//! often it is used), but only while the copy still holds those bytes: one
//! the agent changed is sent as it now is.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The record beside a held copy, in its directory (one file per directory).
pub const HELD: &str = ".held.json";

#[derive(Debug, Serialize, Deserialize)]
struct Held {
    /// The copy's file name in its directory.
    name: String,
    /// The service's file, as a handle (`mcp-file://files/<id>`).
    handle: String,
    /// The bytes' SHA-256 when fetched, in lower-case hex.
    sha256: String,
}

/// Records that `path` is a copy of the service's file `handle`, as it is
/// now.
///
/// # Errors
/// The copy cannot be read or the record written.
pub async fn remember_held(path: &Path, handle: &str) -> std::io::Result<()> {
    let bytes = tokio::fs::read(path).await?;
    let held = Held {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        handle: handle.to_owned(),
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    tokio::fs::write(
        dir.join(HELD),
        serde_json::to_vec(&held).map_err(std::io::Error::other)?,
    )
    .await
}

/// The service's file id `path` is a copy of, when its bytes (`sha256`, in
/// hex) are still the ones fetched; None otherwise.
pub(crate) async fn held_file_id(path: &Path, sha256: &str) -> Option<String> {
    let dir = path.parent()?;
    let raw = tokio::fs::read(dir.join(HELD)).await.ok()?;
    let held: Held = serde_json::from_slice(&raw).ok()?;
    let name = path.file_name()?.to_string_lossy();
    if held.name != name || held.sha256 != sha256 {
        return None;
    }
    let id = held
        .handle
        .strip_prefix(sven_model_drivers::invoke::HANDLE_PREFIX)?;
    let ok = id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    ok.then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11d1032e-7778-4003-9602-b0382067fde3";

    #[tokio::test]
    async fn a_held_copy_is_named_by_its_file_until_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poster.png");
        tokio::fs::write(&path, b"the poster").await.unwrap();
        remember_held(
            &path,
            &format!("{}{ID}", sven_model_drivers::invoke::HANDLE_PREFIX),
        )
        .await
        .unwrap();
        let digest = hex::encode(Sha256::digest(b"the poster"));
        assert_eq!(held_file_id(&path, &digest).await.as_deref(), Some(ID));
        // Changed since: sent as it now is.
        assert_eq!(
            held_file_id(&path, &hex::encode(Sha256::digest(b"edited"))).await,
            None
        );
        // Another file in a directory of no record.
        let other = tempfile::tempdir().unwrap();
        assert_eq!(
            held_file_id(&other.path().join("poster.png"), &digest).await,
            None
        );
    }
}
