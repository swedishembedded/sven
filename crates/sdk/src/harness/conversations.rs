// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements unattended agent runs that leave a complete
// record even when they are killed, for its clients. If your team needs
// expertise in agent pipelines you can procure our services by sending an email
// to info@swedishembedded.com.

//! The conversations of a harness run, as one file.
//!
//! `sven harness run --conversations FILE` writes one JSON line per
//! conversation: `{"index": N, "name": "...", "trajectory": {...}}`, the
//! trajectory being the conversation's own ATIF document. While the run goes,
//! a conversation that changed is appended again with its whole state so far,
//! so a killed run keeps everything up to its last finished item; when the run
//! ends the file is rewritten with exactly one line per conversation. A reader
//! therefore keeps the last line of each `index`. One file per run, never one
//! per conversation: a sweep of thousands of conversations must not cost
//! thousands of inodes.
//!
//! [`read_conversations`] also reads the layout older versions wrote, a
//! directory of `NNNN-<name>.json` files, one ATIF document each.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One conversation of a run: where it sits in the order the conversations
/// began, its name (`<task>#<item index>` inside a `for_each`), and its
/// trajectory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedConversation {
    /// Position in the order the conversations began; stable for the run.
    pub index: usize,
    /// The conversation's name.
    pub name: String,
    /// The conversation as an ATIF document of its own.
    pub trajectory: atif::Trajectory,
}

/// Reads the conversations `path` holds, ordered by `index`: a JSON-lines file
/// written by `--conversations` (the last line of an index wins), or a
/// directory of `NNNN-<name>.json` files from an older version.
///
/// # Errors
///
/// When `path` cannot be read, or a line or file is not a conversation. A
/// torn last line of a killed run's file is ignored, as an append that was
/// never finished.
pub fn read_conversations(path: &Path) -> std::io::Result<Vec<NamedConversation>> {
    let invalid = |what: String| std::io::Error::new(std::io::ErrorKind::InvalidData, what);
    let mut by_index: BTreeMap<usize, NamedConversation> = BTreeMap::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let file = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = file.strip_suffix(".json") else {
                continue;
            };
            let Some((index, name)) = stem.split_once('-') else {
                continue;
            };
            let Ok(index) = index.parse::<usize>() else {
                continue;
            };
            let trajectory = serde_json::from_slice(&std::fs::read(entry.path())?)
                .map_err(|e| invalid(format!("{file}: {e}")))?;
            by_index.insert(
                index,
                NamedConversation {
                    index,
                    name: name.to_string(),
                    trajectory,
                },
            );
        }
    } else {
        let text = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        for (n, line) in lines.iter().enumerate() {
            match serde_json::from_str::<NamedConversation>(line) {
                Ok(conversation) => {
                    by_index.insert(conversation.index, conversation);
                }
                // The line a kill cut short.
                Err(_) if n + 1 == lines.len() && !text.ends_with('\n') => {}
                Err(e) => return Err(invalid(format!("line {}: {e}", n + 1))),
            }
        }
    }
    Ok(by_index.into_values().collect())
}
