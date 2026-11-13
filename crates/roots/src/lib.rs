// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running many independent
// agents on one machine without shared state. If your team needs expertise in
// isolating agent processes then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Where sven keeps its per-user state: configuration, data (sessions,
//! chats, memory), state (audit logs, worktrees), temporary files and the
//! home directory `~` means.
//!
//! By default these are the platform's (`$HOME`, `$XDG_*`). A process given a
//! state root (`sven --state-root DIR`, or [`set_state_root`] from an
//! embedding application) keeps all of them under that one directory instead,
//! reads no `/etc/sven` or user configuration, and so shares no file with any
//! other process that has a root of its own. Every crate asks here rather
//! than for `dirs::*`; `make check/gates/state-roots` holds that.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Keeps all of this process's per-user state under `root`, created (private
/// to the user) with its subdirectories. Once per process; the same root
/// again is fine.
///
/// # Errors
/// The root cannot be made absolute or created, or another root is already
/// set.
pub fn set_state_root(root: &Path) -> Result<(), String> {
    let root =
        std::path::absolute(root).map_err(|e| format!("state root {}: {e}", root.display()))?;
    if let Some(set) = ROOT.get() {
        return if *set == root {
            Ok(())
        } else {
            Err(format!(
                "the state root is already {}; one process has one",
                set.display()
            ))
        };
    }
    for sub in ["home", "config", "data", "state", "tmp"] {
        let dir = root.join(sub);
        std::fs::create_dir_all(&dir).map_err(|e| format!("state root {}: {e}", dir.display()))?;
    }
    restrict(&root);
    let _ = ROOT.set(root);
    Ok(())
}

#[cfg(unix)]
fn restrict(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    // Best effort: the state of an agent is its own.
    let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict(_root: &Path) {}

/// The state root, when one is set.
#[must_use]
pub fn state_root() -> Option<&'static Path> {
    ROOT.get().map(PathBuf::as_path)
}

fn under(sub: &str) -> Option<PathBuf> {
    state_root().map(|root| root.join(sub))
}

/// The directory `~` means.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    under("home").or_else(dirs::home_dir)
}

/// Configuration files (`sven/config.yaml`, credentials, MCP sign-ins).
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    under("config").or_else(dirs::config_dir)
}

/// Durable data: chats, sessions, memory, served skills.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    under("data").or_else(dirs::data_dir)
}

/// Logs and worktrees; where the platform has none, the data directory.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    under("state").or_else(|| dirs::state_dir().or_else(dirs::data_dir))
}

/// Scratch files that are this process's own.
#[must_use]
pub fn temp_dir() -> PathBuf {
    under("tmp").unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test: the root is process-wide and set once.
    #[test]
    fn a_root_holds_every_directory_and_cannot_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agent-1");
        assert!(state_root().is_none());
        set_state_root(&root).unwrap();
        set_state_root(&root).unwrap();
        for (found, sub) in [
            (home_dir().unwrap(), "home"),
            (config_dir().unwrap(), "config"),
            (data_dir().unwrap(), "data"),
            (state_dir().unwrap(), "state"),
            (temp_dir(), "tmp"),
        ] {
            assert_eq!(found, root.join(sub));
            assert!(found.is_dir());
        }
        assert!(set_state_root(&dir.path().join("agent-2")).is_err());
    }
}
