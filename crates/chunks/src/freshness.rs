// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Whether a file-derived chunk still describes the file.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

/// A file's state at one moment: enough to notice that it changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceVersion {
    pub size: u64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: u128,
}

/// The state of a chunk relative to its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// The file is as it was when the chunk was recorded.
    Fresh,
    /// The file changed or is gone (`current` is `None`). The chunk is
    /// evidence of what the file *was*, not of what it is.
    Stale {
        recorded: SourceVersion,
        current: Option<SourceVersion>,
    },
    /// The chunk names a file but no version was recorded, so nothing can be
    /// said. Never reported as fresh.
    Unverifiable,
    /// The chunk is not derived from a file.
    NotFileDerived,
}

/// A path that cannot be inspected.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("path {0:?} is outside the session root")]
    OutsideRoot(String),
    #[error("cannot inspect {path:?}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// Reads the current state of a source file.
pub trait SourceProbe {
    /// The file's current version, `Ok(None)` when it does not exist.
    ///
    /// # Errors
    ///
    /// Fails when `path` is outside the probe's root or cannot be inspected.
    fn version(&self, path: &str) -> Result<Option<SourceVersion>, ProbeError>;
}

/// A [`SourceProbe`] confined to a directory tree.
#[derive(Debug, Clone)]
pub struct FsProbe {
    root: PathBuf,
}

impl FsProbe {
    /// # Errors
    ///
    /// Fails when `root` cannot be resolved.
    pub fn new(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, ProbeError> {
        let candidate = Path::new(path);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            if candidate
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            {
                return Err(ProbeError::OutsideRoot(path.to_string()));
            }
            self.root.join(candidate)
        };
        // A missing file has nothing to canonicalise; its parent decides.
        let anchor = if joined.exists() {
            joined.canonicalize()
        } else {
            joined
                .parent()
                .map_or_else(|| Ok(self.root.clone()), Path::canonicalize)
                .map(|parent| parent.join(joined.file_name().unwrap_or_default()))
        }
        .map_err(|source| ProbeError::Io {
            path: path.to_string(),
            source,
        })?;
        if anchor.starts_with(&self.root) {
            Ok(anchor)
        } else {
            Err(ProbeError::OutsideRoot(path.to_string()))
        }
    }
}

impl SourceProbe for FsProbe {
    fn version(&self, path: &str) -> Result<Option<SourceVersion>, ProbeError> {
        let resolved = self.resolve(path)?;
        match fs::metadata(&resolved) {
            Ok(meta) => {
                let mtime_ns = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                Ok(Some(SourceVersion {
                    size: meta.len(),
                    mtime_ns,
                }))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ProbeError::Io {
                path: path.to_string(),
                source,
            }),
        }
    }
}
