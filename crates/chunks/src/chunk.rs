// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The chunk record and its closed vocabularies.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::freshness::SourceVersion;

/// Bytes of UTF-8 text per estimated token. The token ledger uses the same
/// constant, so a chunk's estimate and the ledger's figure for the same text
/// agree.
pub const BYTES_PER_TOKEN: usize = 4;

/// Hex characters of the content digest kept in a [`ChunkId`].
const ID_HEX_LEN: usize = 16;
const ID_PREFIX: &str = "c-";

/// Full SHA-256 of `bytes`, as lowercase hex.
#[must_use]
pub fn content_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Address of a chunk: `c-` and the first 16 hex characters of the SHA-256 of
/// its content. Identical content has the same id; the id never contains a
/// path separator, so it is safe to use as a file name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChunkId(String);

/// A string that is not a chunk id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a chunk id: {0:?} (expected c- followed by 16 lowercase hex digits)")]
pub struct ChunkIdError(String);

impl ChunkId {
    /// The id of `content`.
    #[must_use]
    pub fn for_content(content: &[u8]) -> Self {
        let digest = content_digest(content);
        Self(format!("{ID_PREFIX}{}", &digest[..ID_HEX_LEN]))
    }

    /// Parse an id received from outside (a tool argument, a stored record).
    ///
    /// # Errors
    ///
    /// Fails unless `text` is `c-` followed by 16 lowercase hex digits.
    pub fn parse(text: &str) -> Result<Self, ChunkIdError> {
        let ok = text.strip_prefix(ID_PREFIX).is_some_and(|hex| {
            hex.len() == ID_HEX_LEN && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if ok {
            Ok(Self(text.to_string()))
        } else {
            Err(ChunkIdError(text.chars().take(40).collect()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ChunkId {
    type Error = ChunkIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ChunkId> for String {
    fn from(id: ChunkId) -> Self {
        id.0
    }
}

/// What a chunk is evidence of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    File,
    CommandOutput,
    TestResult,
    Diff,
    UserConstraint,
    Instruction,
    SubagentFinding,
    Conversation,
}

/// How carefully a chunk's text must be kept out of logs. Ordered: a chunk
/// that is seen twice keeps the stricter label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Internal,
    /// Never written to a log or a trace; only the id, size and digest are.
    Secret,
}

/// How long a chunk is useful. Ordered like [`Sensitivity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    Turn,
    Session,
    Persistent,
}

/// Where a payload lives: its full SHA-256 and its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRef {
    pub digest: String,
    pub bytes: u64,
}

/// Where a chunk came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The tool that produced the text.
    pub source_tool: String,
    /// Digest of the call's canonical arguments.
    pub args_digest: String,
    /// The file the text was read from, relative to the session's root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The state of that file when the text was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version: Option<SourceVersion>,
    /// The turn the text entered the session in.
    pub turn: u32,
}

/// A recorded piece of evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextChunk {
    pub id: ChunkId,
    pub kind: ChunkKind,
    pub content_ref: ContentRef,
    pub provenance: Provenance,
    pub token_estimate: u32,
    pub priority: u8,
    pub sensitivity: Sensitivity,
    pub lifetime: Lifetime,
    /// A pinned chunk is shown in full and its payload stays resident.
    pub pinned: bool,
    /// The text reports a failure (an error result, a failing test).
    pub failure: bool,
}

/// What the caller knows about a payload when it records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDraft {
    pub kind: ChunkKind,
    pub provenance: Provenance,
    pub priority: u8,
    pub sensitivity: Sensitivity,
    pub lifetime: Lifetime,
    pub pinned: bool,
    pub failure: bool,
}

impl ContextChunk {
    pub(crate) fn from_parts(id: ChunkId, digest: String, bytes: usize, draft: ChunkDraft) -> Self {
        Self {
            id,
            kind: draft.kind,
            content_ref: ContentRef {
                digest,
                bytes: bytes as u64,
            },
            provenance: draft.provenance,
            token_estimate: u32::try_from(bytes.div_ceil(BYTES_PER_TOKEN)).unwrap_or(u32::MAX),
            priority: draft.priority,
            sensitivity: draft.sensitivity,
            lifetime: draft.lifetime,
            pinned: draft.pinned,
            failure: draft.failure,
        }
    }

    /// Fold a second sighting of the same content into this record: the later
    /// provenance wins (it names the newest file version) unless it records no
    /// version for the same file, in which case the known version is kept; the
    /// stricter or more important flag wins everywhere else.
    pub(crate) fn merge(&mut self, draft: ChunkDraft) {
        let kept_version = (draft.provenance.source_version.is_none()
            && draft.provenance.path == self.provenance.path)
            .then(|| self.provenance.source_version.take())
            .flatten();
        self.provenance = draft.provenance;
        if kept_version.is_some() {
            self.provenance.source_version = kept_version;
        }
        self.priority = self.priority.max(draft.priority);
        self.sensitivity = self.sensitivity.max(draft.sensitivity);
        self.lifetime = self.lifetime.max(draft.lifetime);
        self.pinned |= draft.pinned;
        self.failure |= draft.failure;
    }
}
