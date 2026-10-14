// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Results that a later message makes redundant.
//!
//! Demoting such a result loses the least: the evidence it held is in the
//! conversation again, or no longer describes the world.
//!
//! - A **superseded** result is a read of a file that was read again whole
//!   (or in the same way), or that a successful edit changed, later on. A
//!   later read of only a part of the file does not supersede an earlier
//!   read of all of it.
//! - A **duplicate** is a result whose text appears again later, byte for
//!   byte.
//!
//! The newest copy is never marked. What is marked is a fact about the
//! conversation, so it does not change as the history grows.

use std::collections::{HashMap, HashSet};

use sven_chunks::{indicates_failure, ChunkId, ChunkKind};
use sven_model::{Message, MessageContent, ToolResultContent};

use crate::assembly::{classify, path_argument};

/// Tools that change the file named by their `path` argument.
const EDITING_TOOLS: [&str; 3] = ["write_file", "edit_file", "delete_file"];

/// The call ids whose results a later message replaces.
#[derive(Debug, Default)]
pub(crate) struct Replaced {
    pub superseded: HashSet<String>,
    pub duplicate: HashSet<String>,
}

fn normal(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

/// Whether `arguments` ask for the whole file: a path and nothing else.
fn whole_file(arguments: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| {
            v.as_object()
                .map(|o| o.keys().all(|k| k == "path" || k == "file_path"))
        })
        .unwrap_or(false)
}

struct Read<'a> {
    id: &'a str,
    path: String,
    arguments: &'a str,
}

/// The results of `messages` that a later message makes redundant.
pub(crate) fn replaced_results(messages: &[Message]) -> Replaced {
    let mut calls: HashMap<&str, (&str, &str)> = HashMap::new();
    let mut reads: Vec<(usize, Read<'_>)> = Vec::new();
    let mut edits: Vec<(usize, String)> = Vec::new();
    let mut copies: HashMap<ChunkId, Vec<&str>> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        match &message.content {
            MessageContent::ToolCall {
                tool_call_id,
                function,
            } => {
                calls.insert(tool_call_id, (&function.name, &function.arguments));
            }
            MessageContent::ToolResult {
                tool_call_id,
                content: ToolResultContent::Text(raw),
            } => {
                let Some(&(name, arguments)) = calls.get(tool_call_id.as_str()) else {
                    continue;
                };
                if EDITING_TOOLS.contains(&name) && !indicates_failure(raw) {
                    if let Some(path) = path_argument(arguments) {
                        edits.push((index, normal(&path)));
                    }
                }
                if classify(name, arguments).is_none() {
                    continue;
                }
                copies
                    .entry(ChunkId::for_content(raw.as_bytes()))
                    .or_default()
                    .push(tool_call_id);
                if classify(name, arguments) == Some(ChunkKind::File) {
                    if let Some(path) = path_argument(arguments) {
                        reads.push((
                            index,
                            Read {
                                id: tool_call_id,
                                path: normal(&path),
                                arguments,
                            },
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let mut replaced = Replaced::default();
    for (n, (index, read)) in reads.iter().enumerate() {
        let read_again = reads[n + 1..].iter().any(|(_, later)| {
            later.path == read.path
                && (later.arguments == read.arguments || whole_file(later.arguments))
        });
        let edited = edits
            .iter()
            .any(|(at, path)| at > index && *path == read.path);
        if read_again || edited {
            replaced.superseded.insert(read.id.to_string());
        }
    }
    for ids in copies.values() {
        for id in &ids[..ids.len().saturating_sub(1)] {
            if !replaced.superseded.contains(*id) {
                replaced.duplicate.insert((*id).to_string());
            }
        }
    }
    replaced
}
