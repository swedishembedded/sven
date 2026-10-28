// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Session-scoped store for streaming output buffers.
//!
//! Unlike the memory-mapped `ContextStore` (`sven-tools-ctx`), an
//! `OutputBuffer` grows incrementally as a subprocess streams bytes into it.
//! The store is designed for two concurrent access patterns:
//!
//! - **Writer** - the `task` or `shell` tool appends bytes and updates status
//!   from a background tokio task.
//! - **Reader** - the model calls `buf_read` / `buf_grep` / `buf_status` to
//!   inspect the buffer contents while it is still growing.
//!
//! Both sides share the same `Arc<Mutex<OutputBufferStore>>`.  Reads take the
//! lock momentarily; writers also lock momentarily on each `append` call.
//! Because both operations are O(data_since_last_append), no long-held locks
//! are needed.
//!
//! # Bounds
//!
//! Every buffer has a capture quota ([`DEFAULT_CAPTURE_QUOTA_BYTES`] unless the
//! creator names one). Bytes past the quota are counted (bytes and lines) but
//! not retained, so a writer can keep draining a pipe without growing memory;
//! the buffer then reports observed against retained totals. A small rolling
//! window of the newest bytes is kept beyond the quota ([`TAIL_WINDOW_BYTES`])
//! because the end of a stream is where errors and summaries are. The whole
//! store is bounded too: creating a buffer evicts the oldest finished buffers
//! once retained bytes exceed [`STORE_RETAINED_BUDGET_BYTES`].
//!
//! Buffers are not spilled to disk; the quota is the memory bound.

use std::collections::HashMap;
use std::time::Instant;

use regex::Regex;

pub use sven_tool_api::grep_match::GrepMatch;

// ─── Public types ─────────────────────────────────────────────────────────────

/// Bytes a buffer retains unless its creator names another quota.
pub const DEFAULT_CAPTURE_QUOTA_BYTES: usize = 16 * 1024 * 1024;

/// Newest bytes kept past the quota so the end of a long stream stays visible.
pub const TAIL_WINDOW_BYTES: usize = 256 * 1024;

/// Retained bytes across all buffers of one store above which creating a
/// buffer evicts the oldest finished ones.
pub const STORE_RETAINED_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// What process created this buffer.
#[derive(Debug, Clone)]
pub enum BufferSource {
    /// A subagent spawned via the `task` tool.
    Subagent {
        prompt: String,
        mode: String,
        description: String,
    },
    /// A shell command spawned via the `shell` tool.
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

/// Summary metadata for a buffer handle.
#[derive(Debug, Clone)]
pub struct BufferMetadata {
    pub handle_id: String,
    /// Bytes retained in the buffer (readable through `buf_read`/`buf_grep`).
    pub total_bytes: usize,
    /// Lines retained in the buffer.
    pub total_lines: usize,
    /// Bytes the writer produced, retained or not.
    pub observed_bytes: usize,
    /// Lines the writer produced, retained or not.
    pub observed_lines: usize,
    pub status: BufferStatus,
    pub elapsed_secs: f32,
    /// Short human-readable description of the source.
    pub description: String,
}

/// A single growing output buffer backed by an in-memory `Vec<u8>`.
pub struct OutputBuffer {
    /// Retained raw bytes (at most `quota`), stdout and stderr interleaved in
    /// arrival order.
    pub(crate) data: Vec<u8>,
    /// Byte offset of the start of each retained line (0-indexed).
    pub(crate) line_index: Vec<usize>,
    pub status: BufferStatus,
    pub created_at: Instant,
    pub source: BufferSource,
    quota: usize,
    observed_bytes: usize,
    observed_newlines: usize,
    last_observed_byte: Option<u8>,
    /// Rolling window of the newest bytes seen after the quota was reached.
    tail_window: Vec<u8>,
    /// True once the window dropped bytes from its front.
    tail_window_trimmed: bool,
}

impl OutputBuffer {
    fn new(source: BufferSource, quota: usize) -> Self {
        Self {
            data: Vec::new(),
            line_index: Vec::new(),
            status: BufferStatus::Running { pid: None },
            created_at: Instant::now(),
            source,
            quota,
            observed_bytes: 0,
            observed_newlines: 0,
            last_observed_byte: None,
            tail_window: Vec::new(),
            tail_window_trimmed: false,
        }
    }

    /// Append bytes: retain up to the quota, count the rest, and index only
    /// the newly retained bytes, so a stream of many small appends costs
    /// O(total bytes), not O(appends x total bytes).
    pub fn append(&mut self, bytes: &[u8]) {
        let Some(&last) = bytes.last() else {
            return;
        };
        self.observed_bytes += bytes.len();
        self.observed_newlines += bytes.iter().filter(|&&b| b == b'\n').count();
        self.last_observed_byte = Some(last);

        let room = self.quota.saturating_sub(self.data.len());
        let (kept, discarded) = bytes.split_at(room.min(bytes.len()));
        if !kept.is_empty() {
            self.index_and_retain(kept);
        }
        if !discarded.is_empty() {
            self.roll_tail_window(discarded);
        }
    }

    fn index_and_retain(&mut self, bytes: &[u8]) {
        let old_len = self.data.len();
        // A line starts at the first byte, and after every newline that has a
        // byte following it (a trailing newline opens no empty line). The
        // newline that ended the previously retained bytes has its follower
        // in `bytes`, so its line start is recorded here.
        if old_len == 0 || self.data[old_len - 1] == b'\n' {
            self.line_index.push(old_len);
        }
        self.data.extend_from_slice(bytes);
        let end = self.data.len();
        for (i, &b) in self.data[old_len..].iter().enumerate() {
            let next = old_len + i + 1;
            if b == b'\n' && next < end {
                self.line_index.push(next);
            }
        }
    }

    fn roll_tail_window(&mut self, bytes: &[u8]) {
        self.tail_window.extend_from_slice(bytes);
        if self.tail_window.len() > 2 * TAIL_WINDOW_BYTES {
            let drop = self.tail_window.len() - TAIL_WINDOW_BYTES;
            self.tail_window.drain(..drop);
            self.tail_window_trimmed = true;
        }
    }

    /// Whether bytes were counted but not retained.
    pub fn is_truncated(&self) -> bool {
        self.observed_bytes > self.data.len()
    }

    /// Lines the writer produced: a trailing partial line counts as a line.
    pub fn observed_lines(&self) -> usize {
        match self.last_observed_byte {
            None => 0,
            Some(b'\n') => self.observed_newlines,
            Some(_) => self.observed_newlines + 1,
        }
    }

    /// The last `n` lines the writer produced, as far as they were kept: the
    /// retained lines while nothing was discarded, else the rolling window
    /// (whose first line is dropped unless it is known to start a line).
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        if !self.is_truncated() {
            let total = self.line_index.len();
            return self.lines(total.saturating_sub(n), total);
        }
        let window = &self.tail_window[..];
        let starts_a_line = !self.tail_window_trimmed && self.data.last() == Some(&b'\n');
        let mut lines: Vec<&[u8]> = window.split(|&b| b == b'\n').collect();
        if window.last() == Some(&b'\n') {
            lines.pop();
        }
        if !starts_a_line && !lines.is_empty() {
            lines.remove(0);
        }
        let from = lines.len().saturating_sub(n);
        lines[from..].iter().map(|l| decode_line(l)).collect()
    }

    /// Retained lines `[start_idx, end_idx)` (0-indexed) as plain text.
    pub fn lines(&self, start_idx: usize, end_idx: usize) -> Vec<String> {
        let end = end_idx.min(self.line_index.len());
        (start_idx..end)
            .map(|i| line_at(&self.data, &self.line_index, i).0)
            .collect()
    }

    fn source_description(&self) -> String {
        match &self.source {
            BufferSource::Subagent { description, .. } => description.clone(),
            BufferSource::Shell { command, .. } => {
                // The command text comes from the model and is arbitrary UTF-8.
                format!("shell: {}", &command[..command.floor_char_boundary(60)])
            }
        }
    }

    pub fn metadata(&self, handle_id: &str) -> BufferMetadata {
        BufferMetadata {
            handle_id: handle_id.to_string(),
            total_bytes: self.data.len(),
            total_lines: self.line_index.len(),
            observed_bytes: self.observed_bytes,
            observed_lines: self.observed_lines(),
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

    /// Create a new buffer with the default capture quota and return its
    /// handle ID.
    pub fn create(&mut self, source: BufferSource) -> String {
        self.create_with_quota(source, DEFAULT_CAPTURE_QUOTA_BYTES)
    }

    /// Create a new buffer that retains at most `quota_bytes` and return its
    /// handle ID. Evicts the oldest finished buffers first when the store as a
    /// whole holds more than [`STORE_RETAINED_BUDGET_BYTES`].
    pub fn create_with_quota(&mut self, source: BufferSource, quota_bytes: usize) -> String {
        self.evict_over_budget();
        let id = self.next_id();
        self.buffers
            .insert(id.clone(), OutputBuffer::new(source, quota_bytes));
        id
    }

    /// Drop the oldest buffers whose writer is done until the store fits its
    /// budget. A running buffer is never dropped: its writer still appends.
    fn evict_over_budget(&mut self) {
        let mut retained: usize = self.buffers.values().map(|b| b.data.len()).sum();
        if retained <= STORE_RETAINED_BUDGET_BYTES {
            return;
        }
        let mut finished: Vec<(u64, String)> = self
            .buffers
            .iter()
            .filter(|(_, b)| !matches!(b.status, BufferStatus::Running { .. }))
            .filter_map(|(id, _)| Some((handle_number(id)?, id.clone())))
            .collect();
        finished.sort();
        for (_, id) in finished {
            if retained <= STORE_RETAINED_BUDGET_BYTES {
                break;
            }
            if let Some(b) = self.buffers.remove(&id) {
                retained -= b.data.len();
            }
        }
    }

    /// Plain-text retained lines `[start_idx, end_idx)` (0-indexed) of a
    /// buffer, or `None` for an unknown handle.
    pub fn lines(&self, handle_id: &str, start_idx: usize, end_idx: usize) -> Option<Vec<String>> {
        self.buffers
            .get(handle_id)
            .map(|b| b.lines(start_idx, end_idx))
    }

    /// The last `n` lines the writer produced (see [`OutputBuffer::tail_lines`]),
    /// or `None` for an unknown handle.
    pub fn tail_lines(&self, handle_id: &str, n: usize) -> Option<Vec<String>> {
        self.buffers.get(handle_id).map(|b| b.tail_lines(n))
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

    /// Return summary metadata for all buffers, sorted by handle ID.
    pub fn list_metadata(&self) -> Vec<BufferMetadata> {
        let mut entries: Vec<BufferMetadata> = self
            .buffers
            .iter()
            .map(|(id, buf)| buf.metadata(id))
            .collect();
        entries.sort_by(|a, b| a.handle_id.cmp(&b.handle_id));
        entries
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

        read_lines_from(&buf.data, &buf.line_index, start_line, end_line).map_err(|e| {
            if buf.is_truncated() {
                format!(
                    "{e} (the buffer retained {} of {} lines; the rest was discarded at the capture quota)",
                    buf.line_index.len(),
                    buf.observed_lines()
                )
            } else {
                e
            }
        })
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

    /// Return all content from a buffer as a plain string.
    /// Returns an empty string if the buffer does not exist.
    pub fn read_all(&self, handle_id: &str) -> Option<String> {
        let buf = self.buffers.get(handle_id)?;
        Some(String::from_utf8_lossy(&buf.data).into_owned())
    }

    /// Return the last `n` lines from a buffer as a plain string (for TUI
    /// streaming preview).  Does not include line-number prefixes.
    pub fn tail(&self, handle_id: &str, n: usize) -> String {
        self.tail_lines(handle_id, n)
            .map(|lines| lines.join("\n"))
            .unwrap_or_default()
    }
}

// ─── Internal line helpers ────────────────────────────────────────────────────

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

/// The numeric part of a `buf_XXXX` handle, for age ordering.
fn handle_number(id: &str) -> Option<u64> {
    u64::from_str_radix(id.strip_prefix("buf_")?, 16).ok()
}

fn decode_line(raw: &[u8]) -> String {
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    String::from_utf8_lossy(raw).into_owned()
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
    (decode_line(raw), byte_end)
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
            file: None,
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

    fn shell_buffer(store: &mut OutputBufferStore, quota: usize) -> String {
        store.create_with_quota(
            BufferSource::Shell {
                command: "x".into(),
                workdir: "/".into(),
            },
            quota,
        )
    }

    #[test]
    fn chunked_appends_index_like_one_append() {
        let text = "alpha\nbeta\n\ngamma\ndelta";
        let mut whole = OutputBufferStore::new();
        let w = shell_buffer(&mut whole, 1 << 20);
        whole.append(&w, text.as_bytes());
        for chunk in [1usize, 2, 3, 7] {
            let mut parts = OutputBufferStore::new();
            let p = shell_buffer(&mut parts, 1 << 20);
            for piece in text.as_bytes().chunks(chunk) {
                parts.append(&p, piece);
            }
            assert_eq!(
                parts.read_range(&p, 1, 99),
                whole.read_range(&w, 1, 99),
                "chunk size {chunk}"
            );
            assert_eq!(parts.metadata(&p).unwrap().total_lines, 5);
        }
    }

    #[test]
    fn trailing_newline_opens_no_empty_line() {
        let (store, id) = make_store_with_buf("a\nb\n");
        let meta = store.metadata(&id).unwrap();
        assert_eq!((meta.total_lines, meta.observed_lines), (2, 2));
    }

    #[test]
    fn quota_retains_a_prefix_and_counts_the_rest() {
        let mut store = OutputBufferStore::new();
        let id = shell_buffer(&mut store, 10);
        for i in 0..100 {
            store.append(&id, format!("line{i}\n").as_bytes());
        }
        let meta = store.metadata(&id).unwrap();
        assert_eq!(meta.total_bytes, 10);
        assert_eq!(meta.observed_lines, 100);
        assert_eq!(meta.observed_bytes, 10 * 6 + 90 * 7);
        assert!(meta.total_lines < meta.observed_lines);
        assert!(store.read_range(&id, 1, 1).unwrap().contains("L1:line0"));
        let err = store.read_range(&id, 50, 60).unwrap_err();
        assert!(err.contains("discarded"), "{err}");
    }

    #[test]
    fn tail_after_quota_is_the_newest_lines() {
        let mut store = OutputBufferStore::new();
        let id = shell_buffer(&mut store, 64);
        for i in 0..1000 {
            store.append(&id, format!("row {i}\n").as_bytes());
        }
        let tail = store.tail_lines(&id, 3).unwrap();
        assert_eq!(tail, ["row 997", "row 998", "row 999"]);
    }

    #[test]
    fn tail_window_stays_bounded() {
        let mut store = OutputBufferStore::new();
        let id = shell_buffer(&mut store, 0);
        let chunk = vec![b'x'; 100_000];
        for _ in 0..50 {
            store.append(&id, &chunk);
        }
        let buf = store.buffers.get(&id).unwrap();
        assert!(buf.tail_window.len() <= 2 * TAIL_WINDOW_BYTES);
        assert_eq!(store.metadata(&id).unwrap().observed_bytes, 5_000_000);
    }

    #[test]
    fn finished_buffers_are_evicted_oldest_first_over_budget() {
        let mut store = OutputBufferStore::new();
        let big = vec![b'x'; STORE_RETAINED_BUDGET_BYTES / 2 + 1];
        let first = shell_buffer(&mut store, usize::MAX);
        store.append(&first, &big);
        store.finish(&first, 0);
        let second = shell_buffer(&mut store, usize::MAX);
        store.append(&second, &big);
        let running = shell_buffer(&mut store, usize::MAX);
        assert!(!store.contains(&first), "oldest finished buffer evicted");
        assert!(store.contains(&second), "a running buffer is never evicted");
        assert!(store.contains(&running));
    }
}
