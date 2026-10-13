// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Deterministic representations of a chunk.
//!
//! [`render`] turns a chunk's raw text into the text a transcript carries at
//! one of four [`Visibility`] levels. No model is involved: every extractor
//! is a fixed function of the text, so the same chunk always renders the same
//! way (which keeps a provider's prompt cache valid across requests).
//!
//! # Invariants
//!
//! - A pinned chunk is only ever [`Visibility::Full`].
//! - A chunk that reports a failure is never below [`Visibility::Long`].
//! - Every form below [`Visibility::Full`] states how much was left out and
//!   the id and tool call that fetch the raw text again.
//! - Chunk text is data. It is quoted, truncated and counted, never
//!   interpreted.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::chunk::{ChunkId, ChunkKind, BYTES_PER_TOKEN};

/// Name of the tool whose [`EXPAND_ACTION`] returns a chunk's raw text.
pub const EXPAND_TOOL_NAME: &str = "context";

/// The action of [`EXPAND_TOOL_NAME`] that returns a chunk.
pub const EXPAND_ACTION: &str = "expand";

const LINE_CAP: usize = 240;
const SHORT_HEAD: usize = 3;
const SHORT_TAIL: usize = 5;
const SHORT_MATCHES: usize = 30;
const LONG_HEAD: usize = 40;
const LONG_TAIL: usize = 40;
const LONG_MATCHES: usize = 200;
const FILE_LONG_HEAD: usize = 30;
const FILE_LONG_TAIL: usize = 10;
const OUTLINE_CAP: usize = 40;
const DIFF_LONG_BODY: usize = 8;

/// How much of a chunk the transcript shows. Ordered: `Hidden` shows least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// A one-line reference.
    Hidden,
    /// Ends, error lines and counts (or the outline of a file or diff).
    Short,
    /// Short with a much wider window.
    Long,
    /// The raw text.
    Full,
}

impl Visibility {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Short => "short",
            Self::Long => "long",
            Self::Full => "full",
        }
    }
}

/// The visibilities a chunk may take, lowest first.
#[must_use]
pub fn allowed_visibilities(failure: bool, pinned: bool) -> Vec<Visibility> {
    const ALL: [Visibility; 4] = [
        Visibility::Hidden,
        Visibility::Short,
        Visibility::Long,
        Visibility::Full,
    ];
    let floor = floor(failure, pinned);
    ALL.into_iter().filter(|v| *v >= floor).collect()
}

fn floor(failure: bool, pinned: bool) -> Visibility {
    if pinned {
        Visibility::Full
    } else if failure {
        Visibility::Long
    } else {
        Visibility::Hidden
    }
}

/// Everything a representation is derived from.
#[derive(Debug, Clone)]
pub struct RenderRequest<'a> {
    pub id: ChunkId,
    pub kind: ChunkKind,
    pub source_tool: &'a str,
    pub path: Option<&'a str>,
    pub failure: bool,
    pub pinned: bool,
    pub raw: &'a str,
}

/// A chunk as the transcript will carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// The visibility actually used: never below the chunk's floor.
    pub visibility: Visibility,
    pub text: String,
}

/// Render `request.raw` at `asked`, raised to the chunk's floor if needed.
#[must_use]
pub fn render(request: &RenderRequest<'_>, asked: Visibility) -> Rendered {
    let visibility = asked.max(floor(request.failure, request.pinned));
    let text = match visibility {
        Visibility::Full => request.raw.to_string(),
        Visibility::Hidden => hidden(request),
        Visibility::Short | Visibility::Long => excerpt(request, visibility),
    };
    Rendered { visibility, text }
}

/// Estimated tokens of `text`, the way the token ledger counts them.
#[must_use]
pub fn tokens_of(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(BYTES_PER_TOKEN)).unwrap_or(u32::MAX)
}

/// Whether a tool result reports a failure: an executor error, a non-zero
/// shell exit, a failing test run or a compiler error.
#[must_use]
pub fn indicates_failure(text: &str) -> bool {
    static FAILURE: OnceLock<Regex> = OnceLock::new();
    let re = FAILURE.get_or_init(|| {
        Regex::new(
            r"(?m)^(?:error: )?\[exit [1-9]\d*\]|^error: |^\s*test result: FAILED|^FAILED\b|^not ok\b|^=+ .*\b[1-9]\d* failed\b|^error(?:\[E\d+\])?:",
        )
        .expect("failure pattern is valid")
    });
    re.is_match(text)
}

fn exit_status(raw: &str) -> Option<i32> {
    static EXIT: OnceLock<Regex> = OnceLock::new();
    let re = EXIT.get_or_init(|| {
        Regex::new(r"^(?:error: )?\[exit (-?\d+)\]").expect("exit pattern is valid")
    });
    re.captures(raw).and_then(|c| c[1].parse().ok())
}

fn expand_hint(id: &ChunkId) -> String {
    format!("{EXPAND_TOOL_NAME} {{\"action\":\"{EXPAND_ACTION}\",\"chunk\":\"{id}\"}}")
}

fn line_count(raw: &str) -> usize {
    raw.lines().count()
}

fn describe(request: &RenderRequest<'_>) -> String {
    let lines = line_count(request.raw);
    format!(
        "{lines} line{}, {} bytes, ~{} tokens",
        if lines == 1 { "" } else { "s" },
        request.raw.len(),
        tokens_of(request.raw)
    )
}

pub(crate) fn kind_name(kind: ChunkKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn hidden(request: &RenderRequest<'_>) -> String {
    format!(
        "[chunk {} hidden | {} | {}; expand: {}]",
        request.id,
        kind_name(request.kind),
        describe(request),
        expand_hint(&request.id)
    )
}

fn header(request: &RenderRequest<'_>, visibility: Visibility) -> String {
    let mut h = format!(
        "[chunk {} | {} | {} | {}",
        request.id,
        visibility.as_str(),
        kind_name(request.kind),
        describe(request)
    );
    if let Some(path) = request.path {
        let _ = write!(h, " | path: {path}");
    }
    if request.source_tool == "shell" {
        let _ = write!(h, " | exit: {}", exit_status(request.raw).unwrap_or(0));
    }
    h.push(']');
    h
}

fn footer(request: &RenderRequest<'_>, omitted_lines: usize, shown_bytes: usize) -> String {
    let omitted_tokens = tokens_of(request.raw).saturating_sub(tokens_of(&"x".repeat(shown_bytes)));
    format!(
        "[omitted {omitted_lines} line{} (~{omitted_tokens} tokens); expand: {}, optional first_line/last_line]",
        if omitted_lines == 1 { "" } else { "s" },
        expand_hint(&request.id)
    )
}

fn excerpt(request: &RenderRequest<'_>, visibility: Visibility) -> String {
    let long = visibility == Visibility::Long;
    let (head, tail, matches) = if long {
        (LONG_HEAD, LONG_TAIL, LONG_MATCHES)
    } else {
        (SHORT_HEAD, SHORT_TAIL, SHORT_MATCHES)
    };
    let lines = numbered_lines(request.raw);
    let mut out = header(request, visibility);
    out.push('\n');
    let mut shown = BTreeSet::new();
    match request.kind {
        ChunkKind::Diff => return diff_excerpt(request, &lines, long, out),
        ChunkKind::File => {
            out.push_str(&outline_block(&lines, &mut shown));
            let (h, t) = if long {
                (FILE_LONG_HEAD, FILE_LONG_TAIL)
            } else {
                (0, 0)
            };
            shown.extend(0..h.min(lines.len()));
            shown.extend(lines.len().saturating_sub(t)..lines.len());
        }
        _ => {
            shown.extend(0..head.min(lines.len()));
            shown.extend(lines.len().saturating_sub(tail)..lines.len());
            let found = signal_lines(&lines);
            out.push_str(&signal_summary(&found));
            shown.extend(found.indexes.iter().copied().take(matches));
            if found.indexes.len() > matches {
                let _ = writeln!(
                    out,
                    "[{} more matching lines not shown]",
                    found.indexes.len() - matches
                );
            }
        }
    }
    let body = render_lines(&lines, &shown);
    let shown_bytes = body.len();
    out.push_str(&body);
    let omitted = lines.len() - shown.len().min(lines.len());
    out.push_str(&footer(request, omitted, shown_bytes));
    out
}

/// `(number, text)` per line. A `L<n>:` prefix, as the file reader writes
/// it, supplies the number; otherwise lines are numbered from 1.
fn numbered_lines(raw: &str) -> Vec<(usize, &str)> {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    let re = PREFIX.get_or_init(|| Regex::new(r"^L(\d+):").expect("line prefix pattern is valid"));
    raw.lines()
        .enumerate()
        .map(|(i, line)| match re.captures(line) {
            Some(c) => (c[1].parse().unwrap_or(i + 1), &line[c[0].len()..]),
            None => (i + 1, line),
        })
        .collect()
}

fn clip(text: &str) -> String {
    if text.chars().count() <= LINE_CAP {
        text.to_string()
    } else {
        let cut: String = text.chars().take(LINE_CAP).collect();
        format!("{cut}…")
    }
}

fn render_lines(lines: &[(usize, &str)], shown: &BTreeSet<usize>) -> String {
    let mut out = String::new();
    let mut previous: Option<usize> = None;
    for &i in shown {
        if previous.is_some_and(|p| i > p + 1) {
            out.push_str("[…]\n");
        }
        let (n, text) = lines[i];
        let _ = writeln!(out, "L{n}: {}", clip(text));
        previous = Some(i);
    }
    out
}

struct Signals {
    indexes: Vec<usize>,
    errors: usize,
    warnings: usize,
}

fn signal_lines(lines: &[(usize, &str)]) -> Signals {
    static ERROR: OnceLock<Regex> = OnceLock::new();
    static WARN: OnceLock<Regex> = OnceLock::new();
    let error = ERROR.get_or_init(|| {
        Regex::new(r"(?i)\b(error|errors|fail|failed|failure|failures|panic|panicked|assert|assertion|exception|traceback|fatal)\b")
            .expect("error pattern is valid")
    });
    let warn = WARN.get_or_init(|| {
        Regex::new(r"(?i)\bwarn(?:ing|ings)?\b").expect("warning pattern is valid")
    });
    let mut s = Signals {
        indexes: Vec::new(),
        errors: 0,
        warnings: 0,
    };
    for (i, (_, text)) in lines.iter().enumerate() {
        if error.is_match(text) {
            s.errors += 1;
            s.indexes.push(i);
        } else if warn.is_match(text) {
            s.warnings += 1;
            s.indexes.push(i);
        }
    }
    s
}

fn signal_summary(s: &Signals) -> String {
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    format!(
        "matched: {}, {}\n",
        plural(s.errors, "error"),
        plural(s.warnings, "warning")
    )
}

fn outline_block(lines: &[(usize, &str)], shown: &mut BTreeSet<usize>) -> String {
    static OUTLINE: OnceLock<Regex> = OnceLock::new();
    let re = OUTLINE.get_or_init(|| {
        Regex::new(
            r"^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|static)\s+)*(?:fn|struct|enum|trait|impl|mod|type|class|def|function|interface|const)\b|^#{1,6}\s",
        )
        .expect("outline pattern is valid")
    });
    let found: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, (_, t))| re.is_match(t))
        .map(|(i, _)| i)
        .collect();
    let mut out = format!("outline: {} entries\n", found.len());
    shown.extend(found.iter().copied().take(OUTLINE_CAP));
    if found.len() > OUTLINE_CAP {
        let _ = writeln!(
            out,
            "[{} more entries not shown]",
            found.len() - OUTLINE_CAP
        );
    }
    out
}

fn diff_excerpt(
    request: &RenderRequest<'_>,
    lines: &[(usize, &str)],
    long: bool,
    mut out: String,
) -> String {
    struct Hunk {
        header: String,
        body: Vec<String>,
    }
    struct FileDiff {
        path: String,
        added: usize,
        removed: usize,
        hunks: Vec<Hunk>,
    }
    let mut files: Vec<FileDiff> = Vec::new();
    for (_, text) in lines {
        if let Some(rest) = text.strip_prefix("diff --git ") {
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, p)| p);
            files.push(FileDiff {
                path: path.to_string(),
                added: 0,
                removed: 0,
                hunks: Vec::new(),
            });
        } else if let Some(file) = files.last_mut() {
            if text.starts_with("@@") {
                file.hunks.push(Hunk {
                    header: clip(text),
                    body: Vec::new(),
                });
                continue;
            }
            if file.hunks.is_empty() {
                continue;
            }
            if text.starts_with('+') {
                file.added += 1;
            } else if text.starts_with('-') {
                file.removed += 1;
            }
            if let Some(hunk) = file.hunks.last_mut() {
                if long && hunk.body.len() < DIFF_LONG_BODY {
                    hunk.body.push(clip(text));
                }
            }
        }
    }
    let _ = writeln!(out, "files ({}):", files.len());
    for f in &files {
        let _ = writeln!(out, "  {} +{} -{}", f.path, f.added, f.removed);
    }
    for f in &files {
        for h in &f.hunks {
            let _ = writeln!(out, "  {}: {}", f.path, h.header);
            for line in &h.body {
                let _ = writeln!(out, "      {line}");
            }
        }
    }
    let shown = out.lines().count().saturating_sub(1);
    let omitted = lines.len().saturating_sub(shown);
    let shown_bytes = out.len();
    out.push_str(&footer(request, omitted, shown_bytes));
    out
}
