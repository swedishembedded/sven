// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Session-scoped store for memory-mapped context handles.
//!
//! Large files and directory trees are opened once and kept as memory-mapped
//! views.  The model receives an opaque handle ID and structural metadata;
//! raw content never enters the LLM context window.  Tools then use the
//! handle to perform zero-copy line-range reads and regex searches.
//!
//! The store is created once per agent session and shared across the context
//! tools via `Arc<Mutex<ContextStore>>` — the same pattern used by
//! [`GdbSessionState`].

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

// ─── Public types ─────────────────────────────────────────────────────────────

/// Summary statistics for a single file within a directory handle.
#[derive(Debug)]
pub struct FileEntry {
    /// Absolute path of this file.
    pub path: PathBuf,
    /// Byte length of this file.
    pub size: usize,
    /// Total number of lines in this file.
    pub line_count: usize,
    /// Memory-mapped view of this file.
    pub mmap: Mmap,
    /// Byte offset of the start of each line (0-indexed).
    pub line_index: Vec<usize>,
}

/// The content kind backing a handle.
pub enum ContextKind {
    /// A single text file.
    SingleFile {
        path: PathBuf,
        mmap: Mmap,
        /// Byte offset of the start of each line (0-indexed).
        line_index: Vec<usize>,
    },
    /// A directory tree (each file memory-mapped separately).
    Directory {
        root: PathBuf,
        files: Vec<FileEntry>,
    },
    /// Aggregated results from a `context_query` call (held in memory).
    Results {
        data: Vec<u8>,
        line_index: Vec<usize>,
        /// Number of per-chunk result entries.
        entry_count: usize,
    },
}

/// Structural metadata returned to the model when a handle is opened.
#[derive(Debug, Clone)]
pub struct ContextMetadata {
    pub handle_id: String,
    /// Total bytes across all mapped content.
    pub total_bytes: usize,
    /// Total lines across all mapped content.
    pub total_lines: usize,
    /// Number of files (1 for single-file handles).
    pub file_count: usize,
    /// Human-readable structural summary (top dirs, extension breakdown, etc.)
    pub summary: String,
}

/// A single open context handle stored inside [`ContextStore`].
pub struct ContextHandle {
    pub id: String,
    pub kind: ContextKind,
    pub metadata: ContextMetadata,
}

/// Session-scoped registry of open context handles.
///
/// All fields are private; use the associated methods to open, query, and
/// access handles.  Callers hold `Arc<Mutex<ContextStore>>` so they can share
/// state across concurrent tool invocations within the same agent session.
pub struct ContextStore {
    handles: HashMap<String, ContextHandle>,
    /// Monotonically increasing counter used to generate unique short IDs.
    counter: u64,
}

impl Default for ContextStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextStore {
    pub fn new() -> Self {
        Self {
            handles: HashMap::new(),
            counter: 0,
        }
    }

    /// Generate a short, unique handle ID.
    fn next_id(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{}_{:04x}", prefix, self.counter)
    }

    // ─── Open operations ──────────────────────────────────────────────────────

    /// Open a single file as a memory-mapped context.
    ///
    /// Returns the new handle ID and its metadata.
    pub fn open_file(&mut self, path: &Path) -> Result<ContextMetadata, String> {
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {}", path.display(), e))?;

        let file = fs::File::open(&canonical)
            .map_err(|e| format!("cannot open {}: {}", canonical.display(), e))?;

        let mmap = unsafe {
            Mmap::map(&file)
                .map_err(|e| format!("mmap failed for {}: {}", canonical.display(), e))?
        };

        let line_index = build_line_index(&mmap);
        let total_lines = line_index.len();
        let total_bytes = mmap.len();

        let id = self.next_id("ctx");
        let summary = format!(
            "Single file: {}\nSize: {} bytes\nLines: {}",
            canonical.display(),
            total_bytes,
            total_lines
        );

        let meta = ContextMetadata {
            handle_id: id.clone(),
            total_bytes,
            total_lines,
            file_count: 1,
            summary,
        };

        self.handles.insert(
            id.clone(),
            ContextHandle {
                id: id.clone(),
                kind: ContextKind::SingleFile {
                    path: canonical,
                    mmap,
                    line_index,
                },
                metadata: meta.clone(),
            },
        );

        Ok(meta)
    }

    /// Open a directory tree as a memory-mapped context.
    ///
    /// Only regular text files are mapped; binary files are skipped with a
    /// note in the summary.  Optionally filtered by a glob include pattern.
    pub fn open_directory(
        &mut self,
        root: &Path,
        include_pattern: Option<&str>,
        recursive: bool,
    ) -> Result<ContextMetadata, String> {
        let canonical = root
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {}", root.display(), e))?;

        if !canonical.is_dir() {
            return Err(format!("{} is not a directory", canonical.display()));
        }

        let glob_pattern: Option<String> = include_pattern.map(str::to_string);

        let mut files: Vec<FileEntry> = Vec::new();
        let mut skipped_binary = 0usize;
        let mut ext_counts: HashMap<String, usize> = HashMap::new();

        collect_files(
            &canonical,
            recursive,
            glob_pattern.as_deref(),
            &mut files,
            &mut skipped_binary,
            &mut ext_counts,
        )?;

        // Sort for deterministic handle order (improves reproducibility).
        files.sort_by(|a, b| a.path.cmp(&b.path));

        let total_bytes: usize = files.iter().map(|f| f.size).sum();
        let total_lines: usize = files.iter().map(|f| f.line_count).sum();
        let file_count = files.len();

        let id = self.next_id("ctx");

        let mut ext_summary: Vec<String> = ext_counts
            .iter()
            .map(|(ext, count)| format!("  .{}: {} files", ext, count))
            .collect();
        ext_summary.sort();

        let top_dirs: Vec<String> = {
            let mut dirs: Vec<PathBuf> = files
                .iter()
                .filter_map(|f| f.path.parent().map(|p| p.to_path_buf()))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            dirs.sort();
            dirs.iter()
                .take(10)
                .map(|d| format!("  {}", d.strip_prefix(&canonical).unwrap_or(d).display()))
                .collect()
        };

        let summary = format!(
            "Directory: {}\nFiles: {} ({} skipped binary)\nTotal: {} bytes, {} lines\n\
             File types:\n{}\nDirectories (up to 10):\n{}",
            canonical.display(),
            file_count,
            skipped_binary,
            total_bytes,
            total_lines,
            ext_summary.join("\n"),
            top_dirs.join("\n")
        );

        let meta = ContextMetadata {
            handle_id: id.clone(),
            total_bytes,
            total_lines,
            file_count,
            summary,
        };

        self.handles.insert(
            id.clone(),
            ContextHandle {
                id: id.clone(),
                kind: ContextKind::Directory {
                    root: canonical,
                    files,
                },
                metadata: meta.clone(),
            },
        );

        Ok(meta)
    }

    /// Register query results produced by `context_query`.
    ///
    /// The results text is held in memory; no temporary file is written.
    pub fn register_results(
        &mut self,
        text: String,
        entry_count: usize,
    ) -> Result<ContextMetadata, String> {
        let data = text.into_bytes();
        let line_index = build_line_index(&data);
        let total_lines = line_index.len();
        let total_bytes = data.len();

        let id = self.next_id("res");
        let summary = format!(
            "Query results: {} entries, {} bytes, {} lines",
            entry_count, total_bytes, total_lines
        );

        let meta = ContextMetadata {
            handle_id: id.clone(),
            total_bytes,
            total_lines,
            file_count: 1,
            summary,
        };

        self.handles.insert(
            id.clone(),
            ContextHandle {
                id: id.clone(),
                kind: ContextKind::Results {
                    data,
                    entry_count,
                    line_index,
                },
                metadata: meta.clone(),
            },
        );

        Ok(meta)
    }

    // ─── Read access ──────────────────────────────────────────────────────────

    /// Read a line range `[start_line, end_line]` (1-indexed, inclusive) from
    /// a handle.  For directory handles, `file_hint` selects a specific file by
    /// path substring; if omitted, all files are concatenated in order (lines
    /// are renumbered globally).
    ///
    /// Returns the formatted text ready to return as tool output.
    pub fn read_range(
        &self,
        handle_id: &str,
        start_line: usize,
        end_line: usize,
        file_hint: Option<&str>,
    ) -> Result<String, String> {
        let handle = self
            .handles
            .get(handle_id)
            .ok_or_else(|| format!("unknown handle '{}'", handle_id))?;

        match &handle.kind {
            ContextKind::SingleFile {
                mmap, line_index, ..
            } => read_lines_from(mmap, line_index, start_line, end_line),
            ContextKind::Results {
                data, line_index, ..
            } => read_lines_from(data, line_index, start_line, end_line),
            ContextKind::Directory { files, .. } => {
                if let Some(hint) = file_hint {
                    let entry = files
                        .iter()
                        .find(|f| f.path.to_string_lossy().contains(hint))
                        .ok_or_else(|| {
                            format!("no file matching '{}' in handle '{}'", hint, handle_id)
                        })?;
                    read_lines_from(&entry.mmap, &entry.line_index, start_line, end_line)
                } else {
                    // No file hint: treat files as concatenated, track global line offset.
                    let mut global_line = 1usize;
                    let mut out = String::new();
                    for entry in files {
                        let file_lines = entry.line_count;
                        let file_end = global_line + file_lines.saturating_sub(1);
                        if file_end < start_line {
                            global_line += file_lines;
                            continue;
                        }
                        if global_line > end_line {
                            break;
                        }
                        // Lines within this file that overlap [start_line, end_line]
                        let local_start = start_line.saturating_sub(global_line) + 1;
                        let local_end = (end_line - global_line + 1).min(file_lines);
                        let chunk = read_lines_from(
                            &entry.mmap,
                            &entry.line_index,
                            local_start,
                            local_end,
                        )?;
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&format!("--- {} ---\n{}", entry.path.display(), chunk));
                        global_line += file_lines;
                    }
                    if out.is_empty() {
                        return Err(format!(
                            "line range {}-{} is out of bounds for handle '{}'",
                            start_line, end_line, handle_id
                        ));
                    }
                    Ok(out)
                }
            }
        }
    }

    // ─── Grep access ──────────────────────────────────────────────────────────

    /// Search all (or a specific) file in the handle for lines matching
    /// `pattern`.  Returns up to `limit` results with `context_lines` of
    /// surrounding context.
    pub fn grep(
        &self,
        handle_id: &str,
        pattern: &str,
        file_hint: Option<&str>,
        context_lines: usize,
        limit: usize,
    ) -> Result<Vec<GrepMatch>, String> {
        let re = regex::Regex::new(pattern)
            .map_err(|e| format!("invalid regex '{}': {}", pattern, e))?;

        let handle = self
            .handles
            .get(handle_id)
            .ok_or_else(|| format!("unknown handle '{}'", handle_id))?;

        let mut results: Vec<GrepMatch> = Vec::new();

        match &handle.kind {
            ContextKind::SingleFile {
                path,
                mmap,
                line_index,
                ..
            } => {
                grep_file(
                    &re,
                    mmap,
                    line_index,
                    path,
                    context_lines,
                    limit,
                    &mut results,
                );
            }
            ContextKind::Results {
                data, line_index, ..
            } => {
                grep_file(
                    &re,
                    data,
                    line_index,
                    Path::new("<results>"),
                    context_lines,
                    limit,
                    &mut results,
                );
            }
            ContextKind::Directory { files, .. } => {
                for entry in files {
                    if limit > 0 && results.len() >= limit {
                        break;
                    }
                    if let Some(hint) = file_hint {
                        if !entry.path.to_string_lossy().contains(hint) {
                            continue;
                        }
                    }
                    let remaining = if limit > 0 {
                        limit - results.len()
                    } else {
                        usize::MAX
                    };
                    grep_file(
                        &re,
                        &entry.mmap,
                        &entry.line_index,
                        &entry.path,
                        context_lines,
                        remaining,
                        &mut results,
                    );
                }
            }
        }

        Ok(results)
    }

    // ─── Raw content access (for context_query chunking) ──────────────────────

    /// Iterate over line-range chunks of `chunk_lines` size from the handle.
    ///
    /// For directory handles, files are concatenated in order.  Each call to
    /// the provided closure receives `(chunk_index, total_chunks, file_label,
    /// content)`.  Stops early if the closure returns `Err`.
    pub fn chunks<F>(&self, handle_id: &str, chunk_lines: usize, mut f: F) -> Result<(), String>
    where
        F: FnMut(usize, usize, &str, &str) -> Result<(), String>,
    {
        let handle = self
            .handles
            .get(handle_id)
            .ok_or_else(|| format!("unknown handle '{}'", handle_id))?;

        match &handle.kind {
            ContextKind::SingleFile {
                path,
                mmap,
                line_index,
                ..
            } => {
                let total_lines = line_index.len();
                if total_lines == 0 {
                    return Ok(());
                }
                let total_chunks = total_lines.div_ceil(chunk_lines);
                for chunk_idx in 0..total_chunks {
                    let start = chunk_idx * chunk_lines + 1;
                    let end = ((chunk_idx + 1) * chunk_lines).min(total_lines);
                    let text = read_lines_from(mmap, line_index, start, end)?;
                    let label = format!("{} L{}-L{}", path.display(), start, end);
                    f(chunk_idx, total_chunks, &label, &text)?;
                }
            }
            ContextKind::Results {
                data, line_index, ..
            } => {
                let total_lines = line_index.len();
                if total_lines == 0 {
                    return Ok(());
                }
                let total_chunks = total_lines.div_ceil(chunk_lines);
                for chunk_idx in 0..total_chunks {
                    let start = chunk_idx * chunk_lines + 1;
                    let end = ((chunk_idx + 1) * chunk_lines).min(total_lines);
                    let text = read_lines_from(data, line_index, start, end)?;
                    let label = format!("<results> L{}-L{}", start, end);
                    f(chunk_idx, total_chunks, &label, &text)?;
                }
            }
            ContextKind::Directory { files, .. } => {
                // Build list of (file_path, content_str) for all files, then chunk globally.
                let contents: Vec<(String, String)> = files
                    .iter()
                    .map(|entry| {
                        let text =
                            read_lines_from(&entry.mmap, &entry.line_index, 1, entry.line_count)
                                .unwrap_or_default();
                        (entry.path.display().to_string(), text)
                    })
                    .collect();

                // Join into one big string and split into chunks by line.
                let all_lines: Vec<&str> =
                    contents.iter().flat_map(|(_, text)| text.lines()).collect();
                let total_lines = all_lines.len();
                if total_lines == 0 {
                    return Ok(());
                }
                let total_chunks = total_lines.div_ceil(chunk_lines);
                let label = format!(
                    "{}/* ({} files)",
                    handle.metadata.summary.lines().next().unwrap_or("dir"),
                    files.len()
                );
                for chunk_idx in 0..total_chunks {
                    let start = chunk_idx * chunk_lines;
                    let end = ((chunk_idx + 1) * chunk_lines).min(total_lines);
                    let chunk_text = all_lines[start..end].join("\n");
                    f(chunk_idx, total_chunks, &label, &chunk_text)?;
                }
            }
        }

        Ok(())
    }

    /// Read all content from a handle as a single string.  Use this for
    /// context_reduce where the full results need to be passed to a sub-query.
    pub fn read_all(&self, handle_id: &str) -> Result<String, String> {
        let handle = self
            .handles
            .get(handle_id)
            .ok_or_else(|| format!("unknown handle '{}'", handle_id))?;

        match &handle.kind {
            ContextKind::SingleFile {
                mmap, line_index, ..
            } => {
                let n = line_index.len();
                if n == 0 {
                    return Ok(String::new());
                }
                read_lines_from(mmap, line_index, 1, n)
            }
            ContextKind::Results {
                data, line_index, ..
            } => {
                let n = line_index.len();
                if n == 0 {
                    return Ok(String::new());
                }
                read_lines_from(data, line_index, 1, n)
            }
            ContextKind::Directory { files, .. } => {
                let mut out = String::new();
                for entry in files {
                    if entry.line_count == 0 {
                        continue;
                    }
                    let chunk =
                        read_lines_from(&entry.mmap, &entry.line_index, 1, entry.line_count)?;
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&format!("--- {} ---\n{}", entry.path.display(), chunk));
                }
                Ok(out)
            }
        }
    }

    /// Return the metadata for a handle without consuming it.
    pub fn metadata(&self, handle_id: &str) -> Option<&ContextMetadata> {
        self.handles.get(handle_id).map(|h| &h.metadata)
    }

    /// Check whether a handle exists.
    pub fn contains(&self, handle_id: &str) -> bool {
        self.handles.contains_key(handle_id)
    }
}

// ─── A single grep match ──────────────────────────────────────────────────────

/// One match returned by [`ContextStore::grep`].
#[derive(Debug, Clone)]
pub struct GrepMatch {
    pub file: PathBuf,
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Build a line-start byte index from a memory map.
///
/// The index contains the byte offset of the start of each line.  Line 0
/// always starts at byte 0.  The index length equals the number of lines.
pub fn build_line_index(data: &[u8]) -> Vec<usize> {
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

/// Extract lines `[start_line, end_line]` (1-indexed, inclusive) from a mmap
/// using the pre-built line index.  Returns formatted `L{n}:content` lines.
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
        let byte_start = line_index[i];
        let byte_end = if i + 1 < line_index.len() {
            line_index[i + 1]
        } else {
            data.len()
        };
        // Strip the trailing newline so the formatted line doesn't double-newline.
        let raw = &data[byte_start..byte_end];
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let text = String::from_utf8_lossy(raw);
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("L{}:{}", i + 1, text));
    }
    Ok(out)
}

/// Search a single file's mmap for `re` matches.  Appends up to `limit` new
/// [`GrepMatch`] entries to `results`.
fn grep_file(
    re: &regex::Regex,
    data: &[u8],
    line_index: &[usize],
    path: &Path,
    context_lines: usize,
    limit: usize,
    results: &mut Vec<GrepMatch>,
) {
    let total = line_index.len();

    for (i, _) in line_index.iter().enumerate() {
        if results.len() >= limit {
            break;
        }
        let byte_start = line_index[i];
        let byte_end = if i + 1 < total {
            line_index[i + 1]
        } else {
            data.len()
        };
        let raw = &data[byte_start..byte_end];
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line_text = String::from_utf8_lossy(raw).into_owned();

        if !re.is_match(&line_text) {
            continue;
        }

        // Collect context lines before the match.
        let ctx_before: Vec<String> = {
            let ctx_start = i.saturating_sub(context_lines);
            (ctx_start..i)
                .map(|j| {
                    let bs = line_index[j];
                    let be = if j + 1 < total {
                        line_index[j + 1]
                    } else {
                        data.len()
                    };
                    let r = &data[bs..be];
                    let r = r.strip_suffix(b"\n").unwrap_or(r);
                    let r = r.strip_suffix(b"\r").unwrap_or(r);
                    String::from_utf8_lossy(r).into_owned()
                })
                .collect()
        };

        // Collect context lines after the match.
        let ctx_after: Vec<String> = {
            let ctx_end = (i + 1 + context_lines).min(total);
            (i + 1..ctx_end)
                .map(|j| {
                    let bs = line_index[j];
                    let be = if j + 1 < total {
                        line_index[j + 1]
                    } else {
                        data.len()
                    };
                    let r = &data[bs..be];
                    let r = r.strip_suffix(b"\n").unwrap_or(r);
                    let r = r.strip_suffix(b"\r").unwrap_or(r);
                    String::from_utf8_lossy(r).into_owned()
                })
                .collect()
        };

        results.push(GrepMatch {
            file: path.to_path_buf(),
            line_number: i + 1,
            line: line_text,
            context_before: ctx_before,
            context_after: ctx_after,
        });
    }
}

/// Simple shell-style wildcard matcher (supports `*` and `?` only).
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = name.chars().collect();
    let mut dp = vec![vec![false; txt.len() + 1]; pat.len() + 1];
    dp[0][0] = true;
    for i in 1..=pat.len() {
        dp[i][0] = dp[i - 1][0] && pat[i - 1] == '*';
    }
    for i in 1..=pat.len() {
        for j in 1..=txt.len() {
            dp[i][j] = match pat[i - 1] {
                '*' => dp[i - 1][j] || dp[i][j - 1],
                '?' => dp[i - 1][j - 1],
                c => dp[i - 1][j - 1] && c == txt[j - 1],
            };
        }
    }
    dp[pat.len()][txt.len()]
}

/// Recursively walk `dir` and build [`FileEntry`] items for each text file.
fn collect_files(
    dir: &Path,
    recursive: bool,
    include_pattern: Option<&str>,
    files: &mut Vec<FileEntry>,
    skipped_binary: &mut usize,
    ext_counts: &mut HashMap<String, usize>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.is_dir() {
            // Skip common noise directories.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(
                name,
                ".git" | "target" | "node_modules" | ".cache" | "__pycache__" | ".venv" | "venv"
            ) {
                continue;
            }
            if recursive {
                collect_files(
                    &path,
                    recursive,
                    include_pattern,
                    files,
                    skipped_binary,
                    ext_counts,
                )?;
            }
            continue;
        }

        if !meta.is_file() {
            continue;
        }

        // Apply include-pattern filter using simple wildcard matching.
        if let Some(pat) = include_pattern {
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !wildcard_matches(pat, file_name) {
                continue;
            }
        }

        // Open and memory-map the file.
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let file_len = meta.len() as usize;
        if file_len == 0 {
            continue;
        }

        let mmap = match unsafe { Mmap::map(&file) } {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Skip binary files.
        let sample_len = mmap.len().min(4096);
        let sample = &mmap[..sample_len];
        if sample.contains(&0u8) {
            *skipped_binary += 1;
            continue;
        }
        let non_printable = sample
            .iter()
            .filter(|&&b| b < 9 || (b > 13 && b < 32))
            .count();
        if non_printable * 100 / sample_len.max(1) > 30 {
            *skipped_binary += 1;
            continue;
        }

        let line_index = build_line_index(&mmap);
        let line_count = line_index.len();

        // Track extension counts for summary.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("(none)")
            .to_lowercase();
        *ext_counts.entry(ext).or_insert(0) += 1;

        files.push(FileEntry {
            path,
            size: file_len,
            line_count,
            mmap,
            line_index,
        });
    }

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    fn write_tmp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn build_line_index_three_lines() {
        let content = b"alpha\nbeta\ngamma\n";
        // Create a temp mmap from bytes.
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();
        let file = std::fs::File::open(f.path()).unwrap();
        let mmap = unsafe { Mmap::map(&file).unwrap() };
        let idx = build_line_index(&mmap);
        // 3 newlines → 3 line starts: 0, 6, 11
        assert_eq!(idx, vec![0, 6, 11]);
    }

    #[test]
    fn open_file_returns_metadata() {
        let tmp = write_tmp("line one\nline two\nline three\n");
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        assert_eq!(meta.file_count, 1);
        assert_eq!(meta.total_lines, 3);
        assert!(meta.total_bytes > 0);
        assert!(!meta.handle_id.is_empty());
    }

    #[test]
    fn read_range_returns_correct_lines() {
        let tmp = write_tmp("alpha\nbeta\ngamma\ndelta\n");
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        let text = store.read_range(&meta.handle_id, 2, 3, None).unwrap();
        assert!(text.contains("L2:beta"), "expected L2:beta in: {text}");
        assert!(text.contains("L3:gamma"), "expected L3:gamma in: {text}");
        assert!(!text.contains("L1:"), "should not include L1");
        assert!(!text.contains("L4:"), "should not include L4");
    }

    #[test]
    fn read_range_out_of_bounds_returns_error() {
        let tmp = write_tmp("only one line\n");
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        let result = store.read_range(&meta.handle_id, 99, 100, None);
        assert!(result.is_err(), "expected error for out-of-bounds range");
    }

    #[test]
    fn grep_finds_matching_lines() {
        let tmp = write_tmp("fn alpha() {}\nfn beta() {}\nlet x = 5;\n");
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        let matches = store.grep(&meta.handle_id, r"^fn ", None, 0, 50).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_number, 1);
        assert_eq!(matches[1].line_number, 2);
    }

    #[test]
    fn grep_context_lines() {
        let tmp = write_tmp("a\nb\nMATCH\nd\ne\n");
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        let matches = store.grep(&meta.handle_id, "MATCH", None, 1, 50).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].context_before, vec!["b"]);
        assert_eq!(matches[0].context_after, vec!["d"]);
    }

    #[test]
    fn grep_limit_respected() {
        let content: String = (0..100).map(|i| format!("match_{}\n", i)).collect();
        let tmp = write_tmp(&content);
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        let matches = store.grep(&meta.handle_id, "match_", None, 0, 10).unwrap();
        assert_eq!(matches.len(), 10);
    }

    #[test]
    fn open_directory_counts_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() {}\nfn c() {}\n").unwrap();
        let mut store = ContextStore::new();
        let meta = store.open_directory(dir.path(), None, true).unwrap();
        assert_eq!(meta.file_count, 2);
        assert_eq!(meta.total_lines, 3);
    }

    #[test]
    fn chunks_produces_correct_count() {
        let content: String = (1..=10).map(|i| format!("line {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        let mut seen = 0usize;
        store
            .chunks(&meta.handle_id, 3, |idx, total, _, _| {
                assert_eq!(total, 4, "10 lines / 3 = 4 chunks");
                assert_eq!(idx, seen);
                seen += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(seen, 4);
    }

    #[test]
    fn unknown_handle_returns_error() {
        let store = ContextStore::new();
        let result = store.read_range("bad_handle", 1, 5, None);
        assert!(result.is_err());
    }
}
