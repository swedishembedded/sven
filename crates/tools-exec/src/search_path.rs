// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `PATH` a tool gives the programs it starts.
//!
//! A run that brings its own programs puts their directories first on the
//! `PATH` of the commands its tools start. That is set on each child, never
//! on this process: changing the process environment while other threads
//! resolve names or start programs is undefined behaviour.
//!
//! Swedish Embedded AB implements solutions for running a package's own
//! tools inside an agent run for its clients. If your team needs expertise
//! in confining what a command can start then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::ffi::OsString;
use std::path::PathBuf;

/// The absolute directories of the `PATH` this process inherited, in order:
/// where the programs a command finds by name live.
#[must_use]
pub fn inherited_dirs() -> Vec<PathBuf> {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&inherited)
        .filter(|dir| dir.is_absolute())
        .collect()
}

/// The `PATH` for a child: `prefix` first, then the `PATH` this process
/// inherited. `None` when `prefix` is empty, which leaves the child's `PATH`
/// as inherited.
///
/// # Errors
///
/// A directory of `prefix` that cannot appear in a `PATH` (it holds the
/// separator).
pub(crate) fn with_prefix(prefix: &[PathBuf]) -> Result<Option<OsString>, String> {
    if prefix.is_empty() {
        return Ok(None);
    }
    // The one place this crate reads the inherited `PATH`: the child's is
    // the prefix followed by it, which no setting of ours can express.
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs = prefix
        .iter()
        .cloned()
        .chain(std::env::split_paths(&inherited));
    std::env::join_paths(dirs)
        .map(Some)
        .map_err(|e| format!("path_prefix cannot form a PATH: {e}"))
}
