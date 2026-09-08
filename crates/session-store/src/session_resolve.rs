// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Resolve a user-supplied session id/prefix/path to an ATIF session file.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::trace_session::session_dir;

/// Resolves a session id to its ATIF `.json` file path.
///
/// Accepts:
/// - Exact session id (filename without `.json`)
/// - Unique id prefix
/// - Absolute or relative filesystem path to a `.json` file
///
/// Only scans the native session directory (`session_dir()`) - legacy YAML
/// chats live elsewhere and are never returned here, since callers of this
/// (headless `--resume`, repointed at `--trace`) cannot read that format;
/// open a legacy chat through `import_legacy_chat_document` first.
///
/// Matches on the *filename*, not the trajectory's own `session_id` field:
/// the TUI writes the trajectory's `session_id` back into the file on every
/// save (see `SessionEntry::to_trajectory`), so the filename is the only
/// value guaranteed to stay stable across saves.
pub fn resolve_session_id(id: &str) -> Result<PathBuf> {
    let p = PathBuf::from(id);
    if p.is_absolute() || id.contains('/') {
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!("file not found: {}", p.display());
    }

    let dir = session_dir();

    let with_ext = dir.join(format!("{id}.json"));
    if with_ext.exists() {
        return Ok(with_ext);
    }

    if dir.exists() {
        let mut matches: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(id) && name.ends_with(".json") {
                matches.push(entry.path());
            }
        }
        matches.sort();
        match matches.len() {
            1 => return Ok(matches.remove(0)),
            n if n > 1 => {
                let ids: Vec<String> = matches
                    .iter()
                    .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
                    .collect();
                anyhow::bail!(
                    "ambiguous id '{}' matches {} sessions:\n  {}\nBe more specific.",
                    id,
                    n,
                    ids.join("\n  ")
                );
            }
            _ => {}
        }
    }

    anyhow::bail!(
        "no session found with id '{}'. Use 'sven chats' to list saved sessions.",
        id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `resolve_session_id` reads the real `session_dir()` (XDG-derived), so
    // these tests serialise on a mutex and point `XDG_DATA_HOME` at a fresh
    // temp dir each time to stay hermetic and safe under parallel test runs.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_session_dir<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialised by ENV_LOCK for the duration of this closure;
        // no other thread in this test binary reads/writes XDG_DATA_HOME
        // without holding the same lock.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let dir = session_dir();
        fs::create_dir_all(&dir).unwrap();
        f(&dir);
        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn resolves_exact_id() {
        with_temp_session_dir(|dir| {
            fs::write(dir.join("abc-123.json"), "{}").unwrap();
            let resolved = resolve_session_id("abc-123").unwrap();
            assert_eq!(resolved, dir.join("abc-123.json"));
        });
    }

    #[test]
    fn resolves_unique_prefix() {
        with_temp_session_dir(|dir| {
            fs::write(dir.join("abc-123-def.json"), "{}").unwrap();
            let resolved = resolve_session_id("abc-123").unwrap();
            assert_eq!(resolved, dir.join("abc-123-def.json"));
        });
    }

    #[test]
    fn ambiguous_prefix_lists_all_matches() {
        with_temp_session_dir(|dir| {
            fs::write(dir.join("abc-111.json"), "{}").unwrap();
            fs::write(dir.join("abc-222.json"), "{}").unwrap();
            let err = resolve_session_id("abc").unwrap_err();
            assert!(err.to_string().contains("ambiguous"));
        });
    }

    #[test]
    fn unknown_id_errors() {
        with_temp_session_dir(|_dir| {
            assert!(resolve_session_id("nonexistent").is_err());
        });
    }

    #[test]
    fn ignores_legacy_yaml_files() {
        with_temp_session_dir(|dir| {
            fs::write(dir.join("abc-123.yaml"), "").unwrap();
            assert!(resolve_session_id("abc-123").is_err());
        });
    }
}
