// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Package resources: the skills and scripts a package carries beside its
//! workflows.
//!
//! A package is more than its workflows when its agents load skills and its
//! gates run scripts: a changed script is a changed program. The files under
//! the package's `skills/` and `bin/` directories therefore travel in the
//! bundle and are pinned by its digest, so one digest names everything a run
//! executes. A host writes them out with [`materialize`] (skills where its
//! agents find skills, `bin/` as the trusted directory its gates run from).
//!
//! The rules are those of imports: paths are relative and stay inside the
//! package, a symlink is refused rather than followed, and the number and
//! size of files are bounded.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in packaging agent workflows
//! with everything they execute then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::HarnessError;

/// The package directories whose files travel with the bundle.
pub const RESOURCE_DIRS: [&str; 2] = ["skills", "bin"];
/// The most files a package carries beside its workflows.
pub const MAX_RESOURCES: usize = 512;
/// The largest single resource file, in bytes.
pub const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024;
/// The most bytes all of a package's resources hold together.
pub const MAX_RESOURCES_TOTAL: u64 = 64 * 1024 * 1024;

/// One file a package carries: its bytes and whether it is executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// The file's bytes, base64 in a serialized bundle.
    #[serde(with = "base64_bytes")]
    pub content: Vec<u8>,
    /// Whether the file is written executable (a script under `bin/`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub executable: bool,
}

impl Resource {
    /// The SHA-256 of the file's bytes, lowercase hex.
    #[must_use]
    pub fn sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.content))
    }
}

fn invalid(message: String) -> HarnessError {
    HarnessError::Invalid(message)
}

/// Checks a resource's package path: relative, under one of
/// [`RESOURCE_DIRS`], every component a plain name.
///
/// # Errors
///
/// [`HarnessError::Invalid`] naming the path and the rule it breaks.
pub fn check_path(path: &str) -> Result<(), HarnessError> {
    let mut parts = path.split('/');
    let top = parts.next().unwrap_or_default();
    if !RESOURCE_DIRS.contains(&top) {
        return Err(invalid(format!(
            "package resource {path} is not under {}",
            RESOURCE_DIRS.join("/ or ")
        )));
    }
    let rest: Vec<&str> = parts.collect();
    let plain = |part: &&str| {
        !part.is_empty()
            && *part != "."
            && *part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if rest.is_empty() || !rest.iter().all(plain) {
        return Err(invalid(format!(
            "package resource {path} must be a relative path of plain names \
             (letters, digits, `.`, `_`, `-`)"
        )));
    }
    Ok(())
}

/// Checks a set of resources against the path rules and the limits.
///
/// # Errors
///
/// [`HarnessError::Invalid`] for the first path or limit broken.
pub fn check(resources: &BTreeMap<String, Resource>) -> Result<(), HarnessError> {
    if resources.len() > MAX_RESOURCES {
        return Err(invalid(format!(
            "a package carries at most {MAX_RESOURCES} resource files, this one {}",
            resources.len()
        )));
    }
    let mut total = 0u64;
    for (path, resource) in resources {
        check_path(path)?;
        let size = resource.content.len() as u64;
        if size > MAX_RESOURCE_BYTES {
            return Err(invalid(format!(
                "package resource {path} is larger than {MAX_RESOURCE_BYTES} bytes"
            )));
        }
        total += size;
    }
    if total > MAX_RESOURCES_TOTAL {
        return Err(invalid(format!(
            "a package's resources hold at most {MAX_RESOURCES_TOTAL} bytes, these {total}"
        )));
    }
    Ok(())
}

/// What pins the resources in a bundle's digest: each path's SHA-256 and
/// whether it is executable.
#[must_use]
pub fn pins(resources: &BTreeMap<String, Resource>) -> serde_json::Value {
    let pinned: BTreeMap<&str, serde_json::Value> = resources
        .iter()
        .map(|(path, resource)| {
            (
                path.as_str(),
                serde_json::json!({ "sha256": resource.sha256(), "executable": resource.executable }),
            )
        })
        .collect();
    serde_json::json!(pinned)
}

/// Whether the package rooted at `root` has a resource directory.
#[must_use]
pub fn has_resources(root: &Path) -> bool {
    RESOURCE_DIRS.iter().any(|dir| root.join(dir).is_dir())
}

/// Reads the resources of the package rooted at `root`: every file under its
/// [`RESOURCE_DIRS`]. A symlink anywhere below them is refused, not followed,
/// so nothing outside the package is read.
///
/// # Errors
///
/// An unreadable file, a symlink or other non-file, a path or limit the
/// rules refuse.
pub fn read(root: &Path) -> Result<BTreeMap<String, Resource>, HarnessError> {
    let io =
        |what: &Path, e: std::io::Error| invalid(format!("cannot read {}: {e}", what.display()));
    let mut found = BTreeMap::new();
    let mut pending: Vec<(String, std::path::PathBuf)> = RESOURCE_DIRS
        .iter()
        .map(|dir| ((*dir).to_string(), root.join(dir)))
        .filter(|(_, path)| path.symlink_metadata().is_ok())
        .collect();
    while let Some((rel, path)) = pending.pop() {
        let meta = path.symlink_metadata().map_err(|e| io(&path, e))?;
        if meta.file_type().is_symlink() {
            return Err(invalid(format!(
                "package resource {rel} is a symlink; a package carries its files themselves"
            )));
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(|e| io(&path, e))? {
                let entry = entry.map_err(|e| io(&path, e))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                pending.push((format!("{rel}/{name}"), entry.path()));
            }
            continue;
        }
        if !meta.is_file() {
            return Err(invalid(format!(
                "package resource {rel} is not a regular file"
            )));
        }
        check_path(&rel)?;
        if meta.len() > MAX_RESOURCE_BYTES {
            return Err(invalid(format!(
                "package resource {rel} is larger than {MAX_RESOURCE_BYTES} bytes"
            )));
        }
        if found.len() >= MAX_RESOURCES {
            return Err(invalid(format!(
                "a package carries at most {MAX_RESOURCES} resource files"
            )));
        }
        let content = std::fs::read(&path).map_err(|e| io(&path, e))?;
        found.insert(
            rel,
            Resource {
                content,
                executable: is_executable(&meta),
            },
        );
    }
    check(&found)?;
    Ok(found)
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Writes `resources` under `dest`, each at its package path, executable
/// ones with mode 0755 and the rest 0644 (on Unix). `dest` is created; a
/// file already there is replaced.
///
/// # Errors
///
/// A path the rules refuse, or a file that cannot be written.
pub fn materialize(
    resources: &BTreeMap<String, Resource>,
    dest: &Path,
) -> Result<(), HarnessError> {
    check(resources)?;
    let io =
        |what: &Path, e: std::io::Error| invalid(format!("cannot write {}: {e}", what.display()));
    for (rel, resource) in resources {
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        }
        std::fs::write(&path, &resource.content).map_err(|e| io(&path, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if resource.executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .map_err(|e| io(&path, e))?;
        }
    }
    Ok(())
}

mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}
