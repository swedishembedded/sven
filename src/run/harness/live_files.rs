// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The files of a run that are written while it goes, not when it ends.
//!
//! `--trace` and `--conversations` are the record of what a run did. A run
//! killed in the middle of a long sweep has no chance to write them at its
//! end, so the trace is appended record by record as the records are observed
//! and each finished item's conversations are written when the item finishes.
//! The files written when the run ends have the same content and replace
//! these.
//!
//! Swedish Embedded AB implements unattended agent runs that leave a complete
//! record even when they are killed, for its clients. If your team needs
//! expertise in agent pipelines you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use sven_sdk::ConversationUpdate;

/// The file a conversation is written to: its position in the order the
/// conversations began, then its name made safe for a file name.
pub(super) fn conversation_file(index: usize, name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{index:04}-{name}.json")
}

/// Appends trace lines to the `--trace` file as they are observed.
pub(super) struct TraceAppender {
    file: Mutex<File>,
}

impl TraceAppender {
    /// Opens `path` for the run: a fresh run starts the file empty, a resumed
    /// one continues what the earlier run left.
    pub(super) fn open(path: &Path, resumed: bool) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// Appends one JSON line, flushed so a kill loses at most a partial line.
    pub(super) fn append(&self, line: &str) {
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(e) = writeln!(file, "{line}").and_then(|()| file.flush()) {
            eprintln!("[sven:harness] warning: cannot append to the trace file: {e}");
        }
    }
}

/// Writes the conversations a finished item changed into `dir`, each through
/// a private file renamed into place.
pub(super) fn write_conversations(dir: &PathBuf, updates: &[ConversationUpdate]) {
    let written = std::fs::create_dir_all(dir).and_then(|()| {
        updates.iter().try_for_each(|update| {
            let target = dir.join(conversation_file(update.index, &update.name));
            let partial = target.with_extension("json.partial");
            let text =
                serde_json::to_string_pretty(&update.trajectory).map_err(std::io::Error::other)?;
            std::fs::write(&partial, text)?;
            std::fs::rename(&partial, &target)
        })
    });
    if let Err(e) = written {
        eprintln!(
            "[sven:harness] warning: cannot keep the conversations in {}: {e}",
            dir.display()
        );
    }
}
