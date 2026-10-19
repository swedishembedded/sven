// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cutting a source into units, and finding a unit again after an edit.

use sven_memory_api::{sha256_hex, Anchor, NewUnit, Span, UnitKind};

/// Names the cutting rules. It is part of the identity of an index generation,
/// so a change of rule is a new generation, never a silent re-cut.
pub const SEGMENTER_VERSION: &str = "sven-sources-1";

/// The most characters a unit holds; a longer block is cut at a line boundary.
const MAX_UNIT_CHARS: usize = 2000;

/// What kind of text a source is, and so how it is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// Paragraphs separated by blank lines.
    Text,
    /// Headings (which give the heading path), fenced code blocks and
    /// paragraphs; a list is one paragraph.
    Markdown,
    /// Top-level items: a blank line followed by an unindented line starts a
    /// new unit, so a blank line inside a body does not split it.
    Code,
}

struct Line<'a> {
    text: &'a str,
    byte: usize,
    chars: usize,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let (mut byte, mut chars) = (0, 0);
    for raw in text.split_inclusive('\n') {
        let body = raw.strip_suffix('\n').unwrap_or(raw);
        out.push(Line {
            text: body,
            byte,
            chars,
        });
        byte += raw.len();
        chars += raw.chars().count();
    }
    out
}

/// A run of lines to become one unit.
struct Block {
    first: usize,
    last: usize,
    kind: UnitKind,
    path: Vec<String>,
}

/// Cuts `text` into units. Deterministic, and every unit is a verbatim slice of
/// `text` by byte and by character range.
#[must_use]
pub fn segment(text: &str, kind: SourceKind) -> Vec<NewUnit> {
    let lines = lines(text);
    let blocks = match kind {
        SourceKind::Text => paragraphs(&lines, |_| false),
        SourceKind::Markdown => markdown(&lines),
        SourceKind::Code => code(&lines),
    };
    let mut ordinals: Vec<(Vec<String>, u32)> = Vec::new();
    let mut units = Vec::new();
    for block in blocks.into_iter().flat_map(|b| cap(&lines, b)) {
        let ordinal = match ordinals.iter_mut().find(|(p, _)| *p == block.path) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                ordinals.push((block.path.clone(), 0));
                0
            }
        };
        let (first, last) = (&lines[block.first], &lines[block.last]);
        let end = last.byte + last.text.len();
        let body = &text[first.byte..end];
        units.push(NewUnit {
            kind: block.kind,
            anchor: Anchor {
                heading_path: block.path,
                block_ordinal: ordinal,
                text_fingerprint: sha256_hex(body.as_bytes()),
            },
            byte_range: Span {
                start: first.byte,
                end,
            },
            char_range: Span {
                start: first.chars,
                end: last.chars + last.text.chars().count(),
            },
            text: body.to_owned(),
        });
    }
    units
}

fn blank(line: &Line<'_>) -> bool {
    line.text.trim().is_empty()
}

/// Runs of non-blank lines, with `breaks` marking a line that starts a block
/// of its own even without a blank line before it.
fn paragraphs(lines: &[Line<'_>], breaks: impl Fn(&Line<'_>) -> bool) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut open: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if blank(line) {
            open = None;
        } else if breaks(line) {
            blocks.push(Block {
                first: i,
                last: i,
                kind: UnitKind::Prose,
                path: Vec::new(),
            });
            open = Some(blocks.len() - 1);
        } else {
            match open {
                Some(b) => blocks[b].last = i,
                None => {
                    blocks.push(Block {
                        first: i,
                        last: i,
                        kind: UnitKind::Prose,
                        path: Vec::new(),
                    });
                    open = Some(blocks.len() - 1);
                }
            }
        }
    }
    blocks
}

fn fence(line: &Line<'_>) -> Option<&'static str> {
    let t = line.text.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn heading(line: &Line<'_>) -> Option<(usize, String)> {
    let hashes = line.text.chars().take_while(|c| *c == '#').count();
    let rest = line.text.get(hashes..)?;
    ((1..=6).contains(&hashes) && rest.starts_with(' ')).then(|| (hashes, rest.trim().to_owned()))
}

fn markdown(lines: &[Line<'_>]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let current =
        |path: &[(usize, String)]| path.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>();
    let mut i = 0;
    let mut prose: Option<usize> = None;
    while i < lines.len() {
        let line = &lines[i];
        if let Some(marker) = fence(line) {
            let mut end = i;
            while end + 1 < lines.len() {
                end += 1;
                if lines[end].text.trim_start().starts_with(marker) {
                    break;
                }
            }
            blocks.push(Block {
                first: i,
                last: end,
                kind: UnitKind::Code,
                path: current(&path),
            });
            prose = None;
            i = end + 1;
        } else if let Some((level, title)) = heading(line) {
            while path.last().is_some_and(|(l, _)| *l >= level) {
                path.pop();
            }
            path.push((level, title));
            blocks.push(Block {
                first: i,
                last: i,
                kind: UnitKind::Heading,
                path: current(&path),
            });
            prose = None;
            i += 1;
        } else if blank(line) {
            prose = None;
            i += 1;
        } else {
            match prose {
                Some(b) => blocks[b].last = i,
                None => {
                    blocks.push(Block {
                        first: i,
                        last: i,
                        kind: UnitKind::Prose,
                        path: current(&path),
                    });
                    prose = Some(blocks.len() - 1);
                }
            }
            i += 1;
        }
    }
    blocks
}

fn indented(line: &Line<'_>) -> bool {
    line.text.starts_with(' ') || line.text.starts_with('\t')
}

fn code(lines: &[Line<'_>]) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut previous_blank = true;
    for (i, line) in lines.iter().enumerate() {
        if blank(line) {
            previous_blank = true;
            continue;
        }
        let closes = line.text.starts_with(['}', ')', ']']);
        let starts_new = blocks.is_empty() || (previous_blank && !indented(line) && !closes);
        if starts_new {
            let signature: String = line
                .text
                .chars()
                .take_while(|c| *c != '{')
                .take(80)
                .collect();
            blocks.push(Block {
                first: i,
                last: i,
                kind: UnitKind::Code,
                path: vec![signature.trim().to_owned()],
            });
        } else if let Some(open) = blocks.last_mut() {
            open.last = i;
        }
        previous_blank = false;
    }
    blocks
}

/// Cuts a block that holds more than [`MAX_UNIT_CHARS`] at line boundaries.
fn cap(lines: &[Line<'_>], block: Block) -> Vec<Block> {
    let mut parts = Vec::new();
    let (mut first, mut size) = (block.first, 0usize);
    for (i, line) in lines
        .iter()
        .enumerate()
        .take(block.last + 1)
        .skip(block.first)
    {
        let len = line.text.chars().count() + 1;
        if size + len > MAX_UNIT_CHARS && i > first {
            parts.push(Block {
                first,
                last: i - 1,
                kind: block.kind,
                path: block.path.clone(),
            });
            first = i;
            size = 0;
        }
        size += len;
    }
    parts.push(Block {
        first,
        last: block.last,
        kind: block.kind,
        path: block.path,
    });
    parts
}

/// Where an anchor points after its source was cut again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reanchored {
    /// Exactly one unit holds the anchored text verbatim; its index.
    Unambiguous(usize),
    /// A person or a model has to decide among these indices: the text occurs
    /// more than once, or it is gone and other words stand in its place.
    NeedsReview(Vec<usize>),
    /// Nothing of it is left.
    Gone,
}

/// Finds the unit `anchor` pointed at among `units`, cut from the edited
/// source. It never guesses: only a verbatim, unique match is unambiguous.
#[must_use]
pub fn reanchor(anchor: &Anchor, units: &[NewUnit]) -> Reanchored {
    let same_text: Vec<usize> = units
        .iter()
        .enumerate()
        .filter(|(_, u)| u.anchor.text_fingerprint == anchor.text_fingerprint)
        .map(|(i, _)| i)
        .collect();
    match same_text.as_slice() {
        [one] => return Reanchored::Unambiguous(*one),
        [] => {}
        _ => return Reanchored::NeedsReview(same_text),
    }
    let same_place: Vec<usize> = units
        .iter()
        .enumerate()
        .filter(|(_, u)| {
            u.anchor.heading_path == anchor.heading_path
                && u.anchor.block_ordinal == anchor.block_ordinal
        })
        .map(|(i, _)| i)
        .collect();
    if same_place.is_empty() {
        Reanchored::Gone
    } else {
        Reanchored::NeedsReview(same_place)
    }
}
