// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The files of a run that are written while it goes, not when it ends.
//!
//! `--trace` and `--conversations` are the record of what a run did. A run
//! killed in the middle of a long sweep has no chance to write them at its
//! end, so the trace is appended record by record as the records are observed
//! and each finished item's conversations are appended when the item finishes.
//! The files written when the run ends have the same content and replace
//! these.
//!
//! Swedish Embedded AB implements unattended agent runs that leave a complete
//! record even when they are killed, for its clients. If your team needs
//! expertise in agent pipelines you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use sven_sdk::ConversationUpdate;

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

/// The line a conversation is written as in the `--conversations` file; see
/// [`sven_sdk::NamedConversation`].
fn conversation_line(
    index: usize,
    name: &str,
    trajectory: &impl serde::Serialize,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&serde_json::json!({
        "index": index,
        "name": name,
        "trajectory": trajectory,
    }))
}

/// Appends the conversations a finished item changed to the `--conversations`
/// file, each with its whole state so far (a reader keeps the last line of an
/// index), and writes the images they refer to beside the file.
pub(super) fn append_conversations(
    file: &TraceAppender,
    path: &Path,
    updates: &[ConversationUpdate],
) {
    let written = updates.iter().try_for_each(|update| {
        if let Some(dir) = path.parent() {
            sven_sdk::write_trajectory_images(dir, &update.images)?;
        }
        let line = conversation_line(update.index, &update.name, &update.trajectory)
            .map_err(std::io::Error::other)?;
        file.append(&line);
        Ok::<(), std::io::Error>(())
    });
    if let Err(e) = written {
        eprintln!(
            "[sven:harness] warning: cannot keep the conversations in {}: {e}",
            path.display()
        );
    }
}

/// The conversations of a finished run as the `--conversations` file's
/// content: one line per conversation, in the order they began.
pub(super) fn final_conversations(
    trajectory: &serde_json::Value,
) -> Result<String, serde_json::Error> {
    let embedded = trajectory["subagent_trajectories"]
        .as_array()
        .into_iter()
        .flatten();
    let mut text = String::new();
    for (index, conversation) in embedded
        .filter(|t| t["extra"]["harness"]["conversation"].is_string())
        .enumerate()
    {
        let name = conversation["extra"]["harness"]["conversation"]
            .as_str()
            .unwrap_or_default();
        text.push_str(&conversation_line(index, name, conversation)?);
        text.push('\n');
    }
    Ok(text)
}
