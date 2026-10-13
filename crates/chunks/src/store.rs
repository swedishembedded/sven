// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The chunk store: payloads on disk, a bounded resident set, an append-only
//! index.
//!
//! # Layout
//!
//! ```text
//! <dir>/index.jsonl        one ContextChunk per line; a later line replaces an
//!                          earlier one with the same id
//! <dir>/payloads/<sha256>  the raw text, named by its full digest
//! ```
//!
//! # Bounds
//!
//! Every payload is written to `payloads/` when it is recorded, so nothing is
//! ever dropped: the store is durable and a session can be restored from the
//! directory. [`StoreLimits::max_resident_bytes`] bounds only the copies kept
//! in memory. When it is exceeded the least recently used *unpinned* payloads
//! leave memory (they are spilled: still on disk, read back on demand). Pinned
//! payloads stay resident even past the bound, so a bound smaller than the
//! pinned set is exceeded by exactly that set. [`StoreLimits::max_chunk_bytes`]
//! bounds one payload; larger content is refused rather than truncated.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::chunk::{ChunkDraft, ChunkId, ContextChunk};
use crate::freshness::{Freshness, SourceProbe};

/// Default bound on payload bytes held in memory.
pub const DEFAULT_MAX_RESIDENT_BYTES: usize = 16 << 20;
/// Default bound on one payload.
pub const DEFAULT_MAX_CHUNK_BYTES: usize = 8 << 20;

const INDEX_FILE: &str = "index.jsonl";
const PAYLOAD_DIR: &str = "payloads";

/// Size bounds of a [`ChunkStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreLimits {
    /// Payload bytes kept in memory; unpinned payloads beyond it are spilled.
    pub max_resident_bytes: usize,
    /// Largest single payload accepted.
    pub max_chunk_bytes: usize,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_resident_bytes: DEFAULT_MAX_RESIDENT_BYTES,
            max_chunk_bytes: DEFAULT_MAX_CHUNK_BYTES,
        }
    }
}

/// The result of recording a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutOutcome {
    pub chunk: ContextChunk,
    /// The same content was already stored; only its record was updated.
    pub deduplicated: bool,
}

struct Resident {
    text: String,
    last_used: u64,
}

/// Content-addressed evidence for one session.
pub struct ChunkStore {
    dir: PathBuf,
    limits: StoreLimits,
    chunks: BTreeMap<ChunkId, ContextChunk>,
    resident: HashMap<ChunkId, Resident>,
    resident_bytes: usize,
    clock: u64,
}

impl ChunkStore {
    /// Open the store in `dir`, creating it, and restore the chunks recorded
    /// there. No payload is loaded until it is read.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be created or its index is unreadable.
    pub fn open(dir: &Path, limits: StoreLimits) -> Result<Self> {
        fs::create_dir_all(dir.join(PAYLOAD_DIR))
            .with_context(|| format!("create chunk store {}", dir.display()))?;
        let mut chunks = BTreeMap::new();
        let index = dir.join(INDEX_FILE);
        if index.exists() {
            let file = File::open(&index).context("open chunk index")?;
            for (n, line) in BufReader::new(file).lines().enumerate() {
                let line = line.context("read chunk index")?;
                if line.trim().is_empty() {
                    continue;
                }
                let chunk: ContextChunk = serde_json::from_str(&line)
                    .with_context(|| format!("chunk index line {}", n + 1))?;
                chunks.insert(chunk.id.clone(), chunk);
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            limits,
            chunks,
            resident: HashMap::new(),
            resident_bytes: 0,
            clock: 0,
        })
    }

    /// Record `content`. Identical content is stored once: the existing record
    /// is updated (see `ContextChunk::merge`) and `deduplicated` is set.
    ///
    /// # Errors
    ///
    /// Fails when `content` exceeds [`StoreLimits::max_chunk_bytes`] or the
    /// payload or index cannot be written.
    pub fn put(&mut self, content: &str, draft: ChunkDraft) -> Result<PutOutcome> {
        if content.len() > self.limits.max_chunk_bytes {
            bail!(
                "content of {} bytes exceeds the chunk limit of {} bytes",
                content.len(),
                self.limits.max_chunk_bytes
            );
        }
        let id = ChunkId::for_content(content.as_bytes());
        let (chunk, deduplicated) = if let Some(existing) = self.chunks.get_mut(&id) {
            existing.merge(draft);
            (existing.clone(), true)
        } else {
            let digest = hex::encode(Sha256::digest(content.as_bytes()));
            self.write_payload(&digest, content)?;
            (
                ContextChunk::from_parts(id.clone(), digest, content.len(), draft),
                false,
            )
        };
        self.append_index(&chunk)?;
        self.chunks.insert(id.clone(), chunk.clone());
        self.admit(&id, content.to_string());
        Ok(PutOutcome {
            chunk,
            deduplicated,
        })
    }

    /// The record of `id`.
    #[must_use]
    pub fn get(&self, id: &ChunkId) -> Option<&ContextChunk> {
        self.chunks.get(id)
    }

    /// Every record, in id order.
    pub fn chunks(&self) -> impl Iterator<Item = &ContextChunk> {
        self.chunks.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Payload bytes currently held in memory.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    #[must_use]
    pub fn is_resident(&self, id: &ChunkId) -> bool {
        self.resident.contains_key(id)
    }

    /// The raw text of `id`, verified against its digest.
    ///
    /// # Errors
    ///
    /// Fails when `id` is unknown, its payload is missing, or the payload no
    /// longer matches its digest (corruption is never returned as evidence).
    pub fn read(&mut self, id: &ChunkId) -> Result<String> {
        let chunk = self
            .chunks
            .get(id)
            .ok_or_else(|| anyhow!("no chunk {id}"))?;
        self.clock += 1;
        if let Some(r) = self.resident.get_mut(id) {
            r.last_used = self.clock;
            return Ok(r.text.clone());
        }
        let digest = chunk.content_ref.digest.clone();
        let path = self.payload_path(&digest);
        let bytes = fs::read(&path).with_context(|| format!("read payload of {id}"))?;
        if hex::encode(Sha256::digest(&bytes)) != digest {
            bail!("payload of {id} does not match its digest");
        }
        let text = String::from_utf8(bytes).map_err(|_| anyhow!("payload of {id} is not UTF-8"))?;
        self.admit(id, text.clone());
        Ok(text)
    }

    /// Lines `first..=last` (1-based) of `id`, newlines kept. A range past
    /// the end yields what exists, possibly nothing.
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read).
    pub fn read_lines(&mut self, id: &ChunkId, first: usize, last: usize) -> Result<String> {
        let text = self.read(id)?;
        let skip = first.max(1) - 1;
        let take = last.saturating_sub(skip);
        Ok(text.split_inclusive('\n').skip(skip).take(take).collect())
    }

    /// Pin or unpin `id`.
    ///
    /// # Errors
    ///
    /// Fails when `id` is unknown or the index cannot be written.
    pub fn set_pinned(&mut self, id: &ChunkId, pinned: bool) -> Result<()> {
        let chunk = self
            .chunks
            .get_mut(id)
            .ok_or_else(|| anyhow!("no chunk {id}"))?;
        chunk.pinned = pinned;
        let chunk = chunk.clone();
        self.append_index(&chunk)
    }

    /// Whether `id` still describes the file it was read from.
    ///
    /// # Errors
    ///
    /// Fails when `id` is unknown or its path cannot be probed.
    pub fn freshness(&self, id: &ChunkId, probe: &dyn SourceProbe) -> Result<Freshness> {
        let chunk = self
            .chunks
            .get(id)
            .ok_or_else(|| anyhow!("no chunk {id}"))?;
        let Some(path) = &chunk.provenance.path else {
            return Ok(Freshness::NotFileDerived);
        };
        let Some(recorded) = chunk.provenance.source_version.clone() else {
            return Ok(Freshness::Unverifiable);
        };
        let current = probe.version(path)?;
        Ok(if current.as_ref() == Some(&recorded) {
            Freshness::Fresh
        } else {
            Freshness::Stale { recorded, current }
        })
    }

    fn payload_path(&self, digest: &str) -> PathBuf {
        self.dir.join(PAYLOAD_DIR).join(digest)
    }

    fn write_payload(&self, digest: &str, content: &str) -> Result<()> {
        let target = self.payload_path(digest);
        if target.exists() {
            return Ok(());
        }
        let tmp = target.with_extension("tmp");
        fs::write(&tmp, content).context("write chunk payload")?;
        fs::rename(&tmp, &target).context("publish chunk payload")
    }

    fn append_index(&self, chunk: &ContextChunk) -> Result<()> {
        let mut line = serde_json::to_string(chunk).context("encode chunk record")?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(INDEX_FILE))
            .context("open chunk index for append")?;
        file.write_all(line.as_bytes())
            .context("append chunk record")
    }

    /// Make `text` resident and evict to the bound.
    fn admit(&mut self, id: &ChunkId, text: String) {
        self.clock += 1;
        let len = text.len();
        if let Some(old) = self.resident.insert(
            id.clone(),
            Resident {
                text,
                last_used: self.clock,
            },
        ) {
            self.resident_bytes -= old.text.len();
        }
        self.resident_bytes += len;
        while self.resident_bytes > self.limits.max_resident_bytes {
            let victim = self
                .resident
                .iter()
                .filter(|(vid, _)| *vid != id && !self.chunks.get(*vid).is_some_and(|c| c.pinned))
                .min_by_key(|(_, r)| r.last_used)
                .map(|(vid, _)| vid.clone());
            let Some(victim) = victim else { break };
            if let Some(r) = self.resident.remove(&victim) {
                self.resident_bytes -= r.text.len();
            }
        }
        // The payload just admitted is itself evictable when it alone is over
        // the bound and unpinned: it was written through, so nothing is lost.
        if self.resident_bytes > self.limits.max_resident_bytes
            && !self.chunks.get(id).is_some_and(|c| c.pinned)
        {
            if let Some(r) = self.resident.remove(id) {
                self.resident_bytes -= r.text.len();
            }
        }
    }
}
