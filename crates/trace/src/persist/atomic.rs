// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Whole-document atomic writer, adapted from `sven-input`'s
//! `chat_document.rs` YAML save path for JSON `Trajectory` documents.
//!
//! Two write entry points, mirroring `save_chat_to` / `save_chat_to_atomic`:
//! - [`write_trajectory`] — a plain `fs::write`, no concurrency guarantees.
//! - [`write_trajectory_atomic`] — temp file + `flock`-guarded sidecar lock
//!   + inode/mtime identity check + atomic `rename`.
//!
//! One deliberate adaptation from `chat_document.rs`: `save_chat_to_atomic`
//! re-stats the target file itself at call-entry, so it only catches a
//! modification that happens *during* the save call (a narrow window).
//! Here, [`write_trajectory_atomic`] instead takes the caller's previously
//! captured [`FileFingerprint`] (from [`read_trajectory_with_fingerprint`])
//! as an explicit `expected` parameter, so it can detect a modification that
//! happened any time between the caller's read and this write — which is
//! the guarantee a "read, edit, save" workflow actually needs.

use std::fs;
use std::path::Path;

use serde::Serialize;
use thiserror::Error;

use crate::model::Trajectory;

/// Errors from the atomic trajectory writer/reader.
#[derive(Debug, Error)]
pub enum PersistError {
    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    /// The file changed (identity or mtime) since the caller's fingerprint
    /// was captured; the write was refused rather than clobbering it.
    #[error("file was modified by another process since it was read")]
    Conflict,
}

/// Identity snapshot of a file used to detect concurrent modification.
///
/// On Unix this is the real `(inode, mtime)` pair. On other platforms it
/// falls back to `(file size, mtime seconds)`, same as `chat_document.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFingerprint {
    identity: u64,
    mtime: i64,
}

fn fingerprint_of(metadata: &fs::Metadata) -> FileFingerprint {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileFingerprint {
            identity: metadata.ino(),
            mtime: metadata.mtime(),
        }
    }
    #[cfg(not(unix))]
    {
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        FileFingerprint {
            identity: metadata.len(),
            mtime,
        }
    }
}

fn serialize_pretty(trajectory: &Trajectory) -> Result<String, PersistError> {
    let mut buf = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::new());
    trajectory.serialize(&mut serializer)?;
    Ok(String::from_utf8(buf).expect("serde_json always produces valid UTF-8"))
}

/// Write a trajectory to `path`, overwriting whatever is there. No atomicity
/// or concurrent-modification guarantees — mirrors `chat_document.rs`'s
/// `save_chat_to`.
pub fn write_trajectory(path: &Path, trajectory: &Trajectory) -> Result<(), PersistError> {
    let content = serialize_pretty(trajectory)?;
    fs::write(path, content)?;
    Ok(())
}

/// Read a trajectory from `path` along with a [`FileFingerprint`] snapshot,
/// for later use with [`write_trajectory_atomic`]'s `expected` parameter.
pub fn read_trajectory_with_fingerprint(path: &Path) -> Result<(Trajectory, FileFingerprint), PersistError> {
    let metadata = fs::metadata(path)?;
    let content = fs::read_to_string(path)?;
    let trajectory: Trajectory = serde_json::from_str(&content)?;
    Ok((trajectory, fingerprint_of(&metadata)))
}

/// Write a trajectory to `path` atomically:
///
/// 1. Serialize to a temp file in the same directory (so `rename` stays on
///    one filesystem).
/// 2. Take an exclusive `flock` on a sidecar lock file.
/// 3. While holding the lock, compare the target's current
///    [`FileFingerprint`] against `expected`. Mismatch (or an unexpected
///    appearance/disappearance of the file) fails with
///    [`PersistError::Conflict`] instead of overwriting.
/// 4. Replace the target via a single `rename()` (atomic on POSIX).
///
/// Pass `expected = None` to skip the concurrent-modification check
/// entirely (first write of a new file, or the caller doesn't care).
/// Otherwise pass the [`FileFingerprint`] returned by an earlier
/// [`read_trajectory_with_fingerprint`] call — a mismatch means someone else
/// wrote to the file after that read.
pub fn write_trajectory_atomic(
    path: &Path,
    trajectory: &Trajectory,
    expected: Option<&FileFingerprint>,
) -> Result<(), PersistError> {
    let content = serialize_pretty(trajectory)?;

    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, &content)?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let lock_path = path.with_extension("json.lock");
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        // SAFETY: `lock_file` stays open (and thus its fd valid) for the
        // duration of the flock/unlock pair; `LockGuard` releases it on drop
        // even if we return early via `?`.
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            let _ = fs::remove_file(&temp_path);
            return Err(std::io::Error::last_os_error().into());
        }
        let _guard = LockGuard(lock_file);

        if let Some(expected) = expected {
            match fs::metadata(path) {
                Ok(current) => {
                    if fingerprint_of(&current) != *expected {
                        let _ = fs::remove_file(&temp_path);
                        return Err(PersistError::Conflict);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // File vanished since the caller's read: also a conflict.
                    let _ = fs::remove_file(&temp_path);
                    return Err(PersistError::Conflict);
                }
                Err(e) => {
                    let _ = fs::remove_file(&temp_path);
                    return Err(e.into());
                }
            }
        }

        fs::rename(&temp_path, path)?;
    }

    #[cfg(not(unix))]
    {
        if let Some(expected) = expected {
            match fs::metadata(path) {
                Ok(current) => {
                    if fingerprint_of(&current) != *expected {
                        let _ = fs::remove_file(&temp_path);
                        return Err(PersistError::Conflict);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let _ = fs::remove_file(&temp_path);
                    return Err(PersistError::Conflict);
                }
                Err(e) => {
                    let _ = fs::remove_file(&temp_path);
                    return Err(e.into());
                }
            }
        }
        fs::rename(&temp_path, path)?;
    }

    Ok(())
}

#[cfg(unix)]
struct LockGuard(fs::File);

#[cfg(unix)]
impl Drop for LockGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `self.0` is a valid, open file descriptor for the
        // lifetime of this guard; unlocking a lock we hold is always safe.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
