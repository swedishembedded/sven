// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared [`GrepMatch`] type used by both the buffer and context stores.

use std::path::PathBuf;

/// One regex match, optionally associated with a source file.
///
/// Buffer grep results have `file = None` (the handle identifies the source).
/// Context grep results have `file = Some(path)` to identify which file in the
/// directory was matched.
#[derive(Debug, Clone)]
pub struct GrepMatch {
    /// Source file path; `None` for buffer-backed results.
    pub file: Option<PathBuf>,
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}
