// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Which lines a shortened form keeps beyond its head and tail.
//!
//! Every function here is a fixed function of the text (and, for a file, of
//! the [`Focus`]): no model is asked, so the same chunk in the same
//! conversation always keeps the same lines. Lines are addressed by their
//! index in the chunk's line list.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::OnceLock;

use regex::Regex;

use crate::focus::Focus;

/// A chunk's lines as `(number, text)`.
pub(crate) type Lines<'a> = [(usize, &'a str)];

/// Lines before an error that are kept as its cause, in a short and a long form.
const CAUSE_SHORT: usize = 2;
const CAUSE_LONG: usize = 5;
/// Lines kept only because they share an identifier with an error.
const RELATED_SHORT: usize = 12;
const RELATED_LONG: usize = 80;
/// Lines added around the places a conversation names in a file.
const PLACES_SHORT: usize = 120;
const PLACES_LONG: usize = 400;
/// Lines of one definition kept because the conversation names it.
const DEFINITION_SHORT: usize = 24;
const DEFINITION_LONG: usize = 60;
/// Names whose definition is searched for in one file.
const MAX_DEFINITIONS: usize = 8;
/// An identifier on more lines than this says nothing about any of them.
const GENERIC_TOKEN_LINES: usize = 25;

/// Whether `text` opens a declaration or a heading.
pub(crate) fn is_outline(text: &str) -> bool {
    static OUTLINE: OnceLock<Regex> = OnceLock::new();
    OUTLINE
        .get_or_init(|| {
            Regex::new(
                r"^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|static)\s+)*(?:fn|struct|enum|trait|impl|mod|type|class|def|function|interface|const)\b|^#{1,6}\s",
            )
            .expect("outline pattern is valid")
        })
        .is_match(text)
}

// ── files ───────────────────────────────────────────────────────────────────

/// Lines of a file kept because the conversation points at them, and why.
#[derive(Debug, Default)]
pub(crate) struct Kept {
    pub indexes: BTreeSet<usize>,
    pub note: String,
}

/// The lines of a file the conversation points at: the lines a place names
/// with the line around them and the declaration they sit in, and the whole
/// definition of every name that the file declares.
pub(crate) fn file_focus(lines: &Lines<'_>, path: Option<&str>, focus: &Focus, long: bool) -> Kept {
    let mut kept = Kept::default();
    let (around, place_cap, definition_cap) = if long {
        (3, PLACES_LONG, DEFINITION_LONG)
    } else {
        (1, PLACES_SHORT, DEFINITION_SHORT)
    };
    let mut notes: Vec<String> = Vec::new();

    let mut named: BTreeSet<usize> = BTreeSet::new();
    for (first, last) in path.map(|p| focus.ranges(p)).unwrap_or_default() {
        let hit: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, (n, _))| *n + around >= first && *n <= last + around)
            .map(|(i, _)| i)
            .collect();
        let Some(&top) = hit.first() else { continue };
        for i in hit {
            if named.len() < place_cap {
                named.insert(i);
            }
        }
        if let Some(declaration) = (0..=top).rev().find(|&i| is_outline(lines[i].1)) {
            named.insert(declaration);
        }
        notes.push(if first == last {
            format!("line {first}")
        } else {
            format!("lines {first}-{last}")
        });
    }
    kept.indexes.extend(named);

    let mut defined = Vec::new();
    for symbol in focus.symbols().take(MAX_DEFINITIONS) {
        if let Some(block) = definition(lines, symbol, definition_cap) {
            defined.push(format!(
                "{symbol} (L{}-L{})",
                lines[block.start].0,
                lines[block.end - 1].0
            ));
            kept.indexes.extend(block);
        }
    }
    if !notes.is_empty() {
        kept.note.push_str(&format!(
            "kept, named in the conversation: {}\n",
            notes.join(", ")
        ));
    }
    if !defined.is_empty() {
        let _ = writeln!(
            kept.note,
            "kept, defined here and named in the conversation: {}",
            defined.join(", ")
        );
    }
    kept
}

/// The index range of the first declaration of `symbol`, through the end of
/// its block, at most `cap` lines.
fn definition(lines: &Lines<'_>, symbol: &str, cap: usize) -> Option<std::ops::Range<usize>> {
    let declaration = Regex::new(&format!(
        r"^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|static)\s+)*(?:fn|struct|enum|trait|type|def|class|function|interface|const|impl)\s+{}\b",
        regex::escape(symbol)
    ))
    .ok()?;
    let start = lines.iter().position(|(_, t)| declaration.is_match(t))?;
    let indent = |t: &str| t.len() - t.trim_start().len();
    let base = indent(lines[start].1);
    let braces = lines[start..lines.len().min(start + 2)]
        .iter()
        .any(|(_, t)| t.contains('{'));
    let mut end = start + 1;
    if braces {
        let mut depth: i64 = 0;
        let mut opened = false;
        for (i, (_, t)) in lines.iter().enumerate().skip(start) {
            for c in t.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            end = i + 1;
            if opened && depth <= 0 {
                break;
            }
        }
    } else {
        while end < lines.len() && (lines[end].1.trim().is_empty() || indent(lines[end].1) > base) {
            end += 1;
        }
        while end > start + 1 && lines[end - 1].1.trim().is_empty() {
            end -= 1;
        }
    }
    Some(start..end.min(start + cap))
}

// ── command output ──────────────────────────────────────────────────────────

/// The lines of an output that report a problem.
pub(crate) struct Signals {
    /// Lines that report an error, in the order they occur.
    pub strong: Vec<usize>,
    /// Lines that mention a problem without being one: a warning, or an
    /// error word at a level that says it is not (`INFO`, `DEBUG`).
    pub weak: Vec<usize>,
    pub errors: usize,
    pub warnings: usize,
}

fn error_pattern() -> &'static Regex {
    static ERROR: OnceLock<Regex> = OnceLock::new();
    ERROR.get_or_init(|| {
        Regex::new(r"(?i)\b(error|errors|fail|failed|failure|failures|panic|panicked|assert|assertion|exception|traceback|fatal)\b")
            .expect("error pattern is valid")
    })
}

/// Whether the line's own log level says it is not an error.
fn below_error_level(text: &str) -> bool {
    static LEVEL: OnceLock<Regex> = OnceLock::new();
    LEVEL
        .get_or_init(|| {
            Regex::new(r"^\W*(?:\d{4}-\d{2}-\d{2}[T ][\d:.,]+Z?\s+)?\[?(?:TRACE|DEBUG|INFO|NOTICE|WARN|WARNING)\b")
                .expect("level pattern is valid")
        })
        .is_match(text)
}

/// Whether the line says there is nothing wrong (`0 failed`, `no errors`).
fn says_none(text: &str) -> bool {
    static NONE: OnceLock<Regex> = OnceLock::new();
    NONE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:0|no|zero|without)\s+(?:errors?|failures?|failed)\b")
            .expect("negation pattern is valid")
    })
    .is_match(text)
}

pub(crate) fn signal_lines(lines: &Lines<'_>) -> Signals {
    static WARN: OnceLock<Regex> = OnceLock::new();
    let warn = WARN.get_or_init(|| {
        Regex::new(r"(?i)\bwarn(?:ing|ings)?\b").expect("warning pattern is valid")
    });
    let mut s = Signals {
        strong: Vec::new(),
        weak: Vec::new(),
        errors: 0,
        warnings: 0,
    };
    for (i, (_, text)) in lines.iter().enumerate() {
        if error_pattern().is_match(text) {
            s.errors += 1;
            if below_error_level(text) || says_none(text) {
                s.weak.push(i);
            } else {
                s.strong.push(i);
            }
        } else if warn.is_match(text) {
            s.warnings += 1;
            s.weak.push(i);
        }
    }
    s
}

pub(crate) fn signal_summary(s: &Signals) -> String {
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    format!(
        "matched: {}, {}\n",
        plural(s.errors, "error"),
        plural(s.warnings, "warning")
    )
}

/// The lines to keep for `signals`, at most `cap` of them: errors first, in
/// order, then the lesser mentions while room is left.
pub(crate) fn chosen_signals(s: &Signals, cap: usize) -> Vec<usize> {
    let mut chosen: Vec<usize> = s.strong.iter().copied().take(cap).collect();
    let room = cap - chosen.len();
    chosen.extend(s.weak.iter().copied().take(room));
    chosen
}

fn identifier_pattern() -> &'static Regex {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    TOKEN.get_or_init(|| {
        Regex::new(
            r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+|[A-Za-z0-9_./-]+\.[A-Za-z0-9]{1,6}:\d+|[A-Za-z0-9]+(?:_[A-Za-z0-9]+){2,}",
        )
        .expect("identifier pattern is valid")
    })
}

/// The cause in front of every kept error line and the lines that share an
/// identifier or a path with one.
pub(crate) fn error_surroundings(
    lines: &Lines<'_>,
    errors: &[usize],
    long: bool,
) -> BTreeSet<usize> {
    let (before, related_cap) = if long {
        (CAUSE_LONG, RELATED_LONG)
    } else {
        (CAUSE_SHORT, RELATED_SHORT)
    };
    let mut kept: BTreeSet<usize> = BTreeSet::new();
    for &e in errors {
        kept.extend(e.saturating_sub(before)..=e);
    }
    let tokens: BTreeSet<&str> = errors
        .iter()
        .flat_map(|&e| {
            identifier_pattern()
                .find_iter(lines[e].1)
                .map(|m| m.as_str())
        })
        .collect();
    if tokens.is_empty() {
        return kept;
    }
    let mut holders: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, (_, text)) in lines.iter().enumerate() {
        for m in identifier_pattern().find_iter(text) {
            if tokens.contains(m.as_str()) {
                holders.entry(m.as_str()).or_default().push(i);
            }
        }
    }
    let mut related: BTreeSet<usize> = BTreeSet::new();
    for indexes in holders.values().filter(|v| v.len() <= GENERIC_TOKEN_LINES) {
        related.extend(indexes.iter().filter(|i| !kept.contains(i)));
    }
    kept.extend(related.into_iter().take(related_cap));
    kept
}

// ── search results ──────────────────────────────────────────────────────────

/// A search result whose lines are `path:line:text`, grouped by file.
pub(crate) struct SearchHits {
    /// `(path, indexes of its lines)` in the order the files first appear.
    pub files: Vec<(String, Vec<usize>)>,
}

/// Groups `lines` by file when most of them look like `path:line:...`.
pub(crate) fn search_hits(lines: &Lines<'_>) -> Option<SearchHits> {
    static HIT: OnceLock<Regex> = OnceLock::new();
    let hit =
        HIT.get_or_init(|| Regex::new(r"^([^\s:][^:]*):\d+[:-]").expect("hit pattern is valid"));
    let mut files: Vec<(String, Vec<usize>)> = Vec::new();
    let mut position: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut matching = 0;
    for (i, (_, text)) in lines.iter().enumerate() {
        let Some(c) = hit.captures(text) else {
            continue;
        };
        matching += 1;
        match position.get(&c[1]) {
            Some(&at) => files[at].1.push(i),
            None => {
                position.insert(c[1].to_string(), files.len());
                files.push((c[1].to_string(), vec![i]));
            }
        }
    }
    (matching * 2 >= lines.len() && files.len() > 1).then_some(SearchHits { files })
}
