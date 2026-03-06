// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Session-scoped store for streaming output buffers.
//!
//! Unlike the memory-mapped [`ContextStore`], an `OutputBuffer` grows
//! incrementally as a subprocess streams bytes into it.  The store is
//! designed for two concurrent access patterns:
//!
//! - **Writer** — the `task` or `shell` tool appends bytes and updates status
//!   from a background tokio task.
//! - **Reader** — the model calls `buf_read` / `buf_grep` / `buf_status` to
//!   inspect the buffer contents while it is still growing.
//!
//! Both sides share the same `Arc<Mutex<OutputBufferStore>>`.  Reads take the
//! lock momentarily; writers also lock momentarily on each `append` call.
//! Because both operations are O(data_since_last_append), no long-held locks
//! are needed.

use std::collections::HashMap;
use std::time::Instant;

use regex::Regex;

// ─── Public types ─────────────────────────────────────────────────────────────

/// What process created this buffer.
#[derive(Debug, Clone)]
pub enum BufferSource {
    /// A subagent spawned via the `task` tool.
    Subagent {
        prompt: String,
        mode: String,
        description: String,
    },
    /// A shell command spawned via the `shell` tool (future use).
    Shell { command: String, workdir: String },
}

/// Lifecycle state of an output buffer.
#[derive(Debug, Clone)]
pub enum BufferStatus {
    /// Process is still running.
    Running {
        /// OS process ID, if known.
        pid: Option<u32>,
    },
    /// Process exited normally.
    Finished { exit_code: i32 },
    /// Process failed to start or was killed with an error.
    Failed { error: String },
}

impl BufferStatus {
    /// One-word label for display.
    pub fn label(&self) -> &'static str {
        match self {
            BufferStatus::Running { .. } => "running",
            BufferStatus::Finished { .. } => "finished",
            BufferStatus::Failed { .. } => "failed",
        }
    }
}

/// One regex match returned by [`OutputBufferStore::grep`].
#[derive(Debug, Clone)]
pub struct GrepMatch {
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Summary metadata for a buffer handle.
#[derive(Debug, Clone)]
pub struct BufferMetadata {
    pub handle_id: String,
    pub total_bytes: usize,
    pub total_lines: usize,
    pub status: BufferStatus,
    pub elapsed_secs: f32,
    /// Short human-readable description of the source.
    pub description: String,
}

/// A single growing output buffer backed by an in-memory `Vec<u8>`.
pub struct OutputBuffer {
    /// Accumulated raw bytes from the subprocess stdout (and stderr mixed in).
    pub data: Vec<u8>,
    /// Byte offset of the start of each line (0-indexed).
    pub line_index: Vec<usize>,
    pub status: BufferStatus,
    pub created_at: Instant,
    pub source: BufferSource,
}

impl OutputBuffer {
    fn new(source: BufferSource) -> Self {
        Self {
            data: Vec::new(),
            line_index: Vec::new(),
            status: BufferStatus::Running { pid: None },
            created_at: Instant::now(),
            source,
        }
    }

    /// Append bytes and update the line index.
    ///
    /// Rebuilds the line index over all accumulated data after each append.
    /// This is O(total_bytes) per call but correct for all cases, including
    /// chunks that end exactly at a `\n` boundary.
    pub fn append(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.data.extend_from_slice(bytes);
        self.line_index = build_line_index(&self.data);
    }

    fn source_description(&self) -> String {
        match &self.source {
            BufferSource::Subagent { description, .. } => description.clone(),
            BufferSource::Shell { command, .. } => {
                format!("shell: {}", &command[..command.len().min(60)])
            }
        }
    }

    pub fn metadata(&self, handle_id: &str) -> BufferMetadata {
        BufferMetadata {
            handle_id: handle_id.to_string(),
            total_bytes: self.data.len(),
            total_lines: self.line_index.len(),
            status: self.status.clone(),
            elapsed_secs: self.created_at.elapsed().as_secs_f32(),
            description: self.source_description(),
        }
    }
}

/// Session-scoped registry of streaming output buffers.
pub struct OutputBufferStore {
    buffers: HashMap<String, OutputBuffer>,
    counter: u64,
}

impl Default for OutputBufferStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputBufferStore {
    pub fn new() -> Self {
        Self {
            buffers: HashMap::new(),
            counter: 0,
        }
    }

    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("buf_{:04x}", self.counter)
    }

    /// Create a new buffer and return its handle ID.
    pub fn create(&mut self, source: BufferSource) -> String {
        let id = self.next_id();
        self.buffers.insert(id.clone(), OutputBuffer::new(source));
        id
    }

    /// Append bytes to an existing buffer.  No-ops for unknown handles.
    pub fn append(&mut self, handle_id: &str, bytes: &[u8]) {
        if let Some(buf) = self.buffers.get_mut(handle_id) {
            buf.append(bytes);
        }
    }

    /// Record the PID once the process has started.
    pub fn set_pid(&mut self, handle_id: &str, pid: u32) {
        if let Some(buf) = self.buffers.get_mut(handle_id) {
            buf.status = BufferStatus::Running { pid: Some(pid) };
        }
    }

    /// Mark a buffer as finished with an exit code.
    pub fn finish(&mut self, handle_id: &str, exit_code: i32) {
        if let Some(buf) = self.buffers.get_mut(handle_id) {
            buf.status = BufferStatus::Finished { exit_code };
        }
    }

    /// Mark a buffer as failed with an error message.
    pub fn fail(&mut self, handle_id: &str, error: String) {
        if let Some(buf) = self.buffers.get_mut(handle_id) {
            buf.status = BufferStatus::Failed { error };
        }
    }

    /// Return summary metadata for a buffer.
    pub fn metadata(&self, handle_id: &str) -> Option<BufferMetadata> {
        self.buffers.get(handle_id).map(|b| b.metadata(handle_id))
    }

    /// Check whether a handle exists.
    pub fn contains(&self, handle_id: &str) -> bool {
        self.buffers.contains_key(handle_id)
    }

    /// Read a line range `[start_line, end_line]` (1-indexed, inclusive).
    ///
    /// Returns formatted `L{n}:{content}` lines, the same format as
    /// `context_read`.
    pub fn read_range(
        &self,
        handle_id: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<String, String> {
        let buf = self
            .buffers
            .get(handle_id)
            .ok_or_else(|| format!("unknown buffer handle '{}'", handle_id))?;

        read_lines_from(&buf.data, &buf.line_index, start_line, end_line)
    }

    /// Regex-search the buffer for up to `limit` matches with `context_lines`
    /// of surrounding context on each side.
    pub fn grep(
        &self,
        handle_id: &str,
        pattern: &str,
        context_lines: usize,
        limit: usize,
    ) -> Result<Vec<GrepMatch>, String> {
        let buf = self
            .buffers
            .get(handle_id)
            .ok_or_else(|| format!("unknown buffer handle '{}'", handle_id))?;

        let re = Regex::new(pattern).map_err(|e| format!("invalid regex '{}': {}", pattern, e))?;

        let mut results = Vec::new();
        grep_buffer(
            &re,
            &buf.data,
            &buf.line_index,
            context_lines,
            limit,
            &mut results,
        );
        Ok(results)
    }

    /// Return the last `n` lines from a buffer as a plain string (for TUI
    /// streaming preview).  Does not include line-number prefixes.
    pub fn tail(&self, handle_id: &str, n: usize) -> String {
        let buf = match self.buffers.get(handle_id) {
            Some(b) => b,
            None => return String::new(),
        };
        if buf.line_index.is_empty() {
            return String::new();
        }
        let total = buf.line_index.len();
        let start_idx = total.saturating_sub(n);
        extract_lines(&buf.data, &buf.line_index, start_idx, total)
    }
}

// ─── Internal line helpers ────────────────────────────────────────────────────

/// Build a line-start byte index from a byte slice.
///
/// The returned vector contains the byte offset of the first byte of each line.
/// Line 0 always starts at byte 0.  A trailing `\n` at the very end of `data`
/// does NOT add an extra empty-line entry (matching the context/store behaviour).
fn build_line_index(data: &[u8]) -> Vec<usize> {
    if data.is_empty() {
        return vec![];
    }
    let mut idx = vec![0usize];
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' && i + 1 < data.len() {
            idx.push(i + 1);
        }
    }
    idx
}

/// Extract lines `[start_line, end_line]` (1-indexed, inclusive) and return
/// them formatted as `L{n}:{content}\n...`.
fn read_lines_from(
    data: &[u8],
    line_index: &[usize],
    start_line: usize,
    end_line: usize,
) -> Result<String, String> {
    if line_index.is_empty() {
        return Ok(String::new());
    }
    let start = start_line.saturating_sub(1);
    let end = end_line.min(line_index.len());
    if start >= line_index.len() {
        return Err(format!(
            "start_line {} exceeds total lines {}",
            start_line,
            line_index.len()
        ));
    }
    let mut out = String::new();
    for i in start..end {
        let (text, _) = line_at(data, line_index, i);
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("L{}:{}", i + 1, text));
    }
    Ok(out)
}

/// Extract lines [start_idx, end_idx) (0-indexed) as plain strings joined by
/// newlines (used by `tail`).
fn extract_lines(data: &[u8], line_index: &[usize], start_idx: usize, end_idx: usize) -> String {
    let mut parts = Vec::with_capacity(end_idx.saturating_sub(start_idx));
    for i in start_idx..end_idx {
        let (text, _) = line_at(data, line_index, i);
        parts.push(text);
    }
    parts.join("\n")
}

/// Return the text of line `idx` (0-indexed) stripped of CR/LF.
fn line_at(data: &[u8], line_index: &[usize], idx: usize) -> (String, usize) {
    let byte_start = line_index[idx];
    let byte_end = if idx + 1 < line_index.len() {
        line_index[idx + 1]
    } else {
        data.len()
    };
    let raw = &data[byte_start..byte_end];
    let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    (String::from_utf8_lossy(raw).into_owned(), byte_end)
}

/// Regex-search a buffer; append up to `limit` new `GrepMatch` entries to
/// `results`.
fn grep_buffer(
    re: &Regex,
    data: &[u8],
    line_index: &[usize],
    context_lines: usize,
    limit: usize,
    results: &mut Vec<GrepMatch>,
) {
    let total = line_index.len();
    for i in 0..total {
        if results.len() >= limit {
            break;
        }
        let (line_text, _) = line_at(data, line_index, i);
        if !re.is_match(&line_text) {
            continue;
        }

        let ctx_before: Vec<String> = {
            let from = i.saturating_sub(context_lines);
            (from..i).map(|j| line_at(data, line_index, j).0).collect()
        };
        let ctx_after: Vec<String> = {
            let to = (i + 1 + context_lines).min(total);
            (i + 1..to)
                .map(|j| line_at(data, line_index, j).0)
                .collect()
        };

        results.push(GrepMatch {
            line_number: i + 1,
            line: line_text,
            context_before: ctx_before,
            context_after: ctx_after,
        });
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store_with_buf(content: &str) -> (OutputBufferStore, String) {
        let mut store = OutputBufferStore::new();
        let id = store.create(BufferSource::Subagent {
            prompt: "test".into(),
            mode: "agent".into(),
            description: "test buf".into(),
        });
        store.append(&id, content.as_bytes());
        store.finish(&id, 0);
        (store, id)
    }

    #[test]
    fn append_and_read() {
        let (store, id) = make_store_with_buf("line one\nline two\nline three\n");
        let out = store.read_range(&id, 1, 3).unwrap();
        assert!(out.contains("L1:line one"), "{out}");
        assert!(out.contains("L2:line two"), "{out}");
        assert!(out.contains("L3:line three"), "{out}");
    }

    #[test]
    fn incremental_append_builds_index() {
        let mut store = OutputBufferStore::new();
        let id = store.create(BufferSource::Shell {
            command: "echo".into(),
            workdir: "/".into(),
        });
        store.append(&id, b"hello\n");
        store.append(&id, b"world\n");
        let out = store.read_range(&id, 1, 2).unwrap();
        assert!(out.contains("L1:hello"), "{out}");
        assert!(out.contains("L2:world"), "{out}");
    }

    #[test]
    fn grep_finds_matches() {
        let (store, id) = make_store_with_buf("fn alpha() {}\nlet x = 1;\nfn beta() {}\n");
        let matches = store.grep(&id, r"^fn ", 0, 50).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_number, 1);
        assert_eq!(matches[1].line_number, 3);
    }

    #[test]
    fn grep_no_matches_is_empty() {
        let (store, id) = make_store_with_buf("hello world\n");
        let matches = store.grep(&id, "xyzzy", 0, 50).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn grep_invalid_regex_errors() {
        let (store, id) = make_store_with_buf("test\n");
        assert!(store.grep(&id, "[invalid", 0, 50).is_err());
    }

    #[test]
    fn metadata_reflects_state() {
        let mut store = OutputBufferStore::new();
        let id = store.create(BufferSource::Subagent {
            prompt: "p".into(),
            mode: "agent".into(),
            description: "desc".into(),
        });
        store.append(&id, b"a\nb\n");
        let meta = store.metadata(&id).unwrap();
        assert_eq!(meta.total_lines, 2);
        assert!(matches!(meta.status, BufferStatus::Running { .. }));
        store.finish(&id, 0);
        let meta = store.metadata(&id).unwrap();
        assert!(matches!(
            meta.status,
            BufferStatus::Finished { exit_code: 0 }
        ));
    }

    #[test]
    fn tail_returns_last_n_lines() {
        let (store, id) = make_store_with_buf("a\nb\nc\nd\ne\n");
        let t = store.tail(&id, 3);
        assert!(t.contains("c"), "{t}");
        assert!(t.contains("d"), "{t}");
        assert!(t.contains("e"), "{t}");
        assert!(!t.contains("a"), "{t}");
    }

    #[test]
    fn unknown_handle_errors() {
        let store = OutputBufferStore::new();
        assert!(store.read_range("nope", 1, 5).is_err());
        assert!(store.grep("nope", "x", 0, 10).is_err());
        assert!(store.metadata("nope").is_none());
    }
}
