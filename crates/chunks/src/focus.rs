// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! What a conversation points at.
//!
//! A [`Focus`] is read from the text of a conversation: the `path:line`
//! places an error, a search result or a call names, and the identifiers the
//! user put in backticks or a search looked for. A shortened form of a chunk
//! keeps what the focus points at, so the part of a file or a log that the
//! work is about survives where a fixed head and tail would not.
//!
//! The text is untrusted. It is scanned for places and names, never
//! interpreted, and a focus is bounded: past [`MAX_ENTRIES`] places and names
//! nothing more is taken, so what is kept does not depend on how long the
//! conversation grows.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

/// The most places and names a focus holds.
pub const MAX_ENTRIES: usize = 512;

/// An identifier shorter than this names nothing in particular.
const MIN_SYMBOL_LEN: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    path: String,
    first: usize,
    last: usize,
}

/// The places and names a conversation points at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Focus {
    places: Vec<Place>,
    symbols: BTreeSet<String>,
}

fn place_pattern() -> &'static Regex {
    static PLACE: OnceLock<Regex> = OnceLock::new();
    PLACE.get_or_init(|| {
        Regex::new(r"([A-Za-z0-9_./\\-]*[A-Za-z0-9_]\.[A-Za-z0-9]{1,6}):(\d+)(?:-(\d+)|:\d+)?")
            .expect("place pattern is valid")
    })
}

fn quoted_pattern() -> &'static Regex {
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    QUOTED.get_or_init(|| {
        Regex::new(r"`([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`")
            .expect("quoted identifier pattern is valid")
    })
}

/// Whether two paths name the same file when one is given shorter.
fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_start_matches("./"), b.trim_start_matches("./"));
    a == b
        || a.strip_suffix(b).is_some_and(|p| p.ends_with('/'))
        || b.strip_suffix(a).is_some_and(|p| p.ends_with('/'))
}

impl Focus {
    /// Reads the `path:line`, `path:line:column` and `path:first-last` places
    /// and the backticked identifiers of `text`.
    pub fn observe(&mut self, text: &str) {
        for c in place_pattern().captures_iter(text) {
            let first: usize = c[2].parse().unwrap_or(0);
            let last = c
                .get(3)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(first);
            if first > 0 && last >= first {
                self.add_place(Place {
                    path: c[1].to_string(),
                    first,
                    last,
                });
            }
        }
        for c in quoted_pattern().captures_iter(text) {
            self.observe_symbol(c[1].rsplit("::").next().unwrap_or(&c[1]));
        }
    }

    /// Notes that the conversation is about `symbol`, an identifier.
    pub fn observe_symbol(&mut self, symbol: &str) {
        let identifier = symbol
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && symbol.chars().all(|c| c.is_alphanumeric() || c == '_');
        if identifier && symbol.len() >= MIN_SYMBOL_LEN && self.len() < MAX_ENTRIES {
            self.symbols.insert(symbol.to_string());
        }
    }

    fn add_place(&mut self, place: Place) {
        if self.len() < MAX_ENTRIES && !self.places.contains(&place) {
            self.places.push(place);
        }
    }

    /// Places and names held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.places.len() + self.symbols.len()
    }

    /// Whether the conversation points at nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether a place names `line` of `path`.
    #[must_use]
    pub fn mentions(&self, path: &str, line: usize) -> bool {
        self.ranges(path)
            .iter()
            .any(|&(a, b)| (a..=b).contains(&line))
    }

    /// The line ranges that name `path`, in the order they were seen.
    #[must_use]
    pub fn ranges(&self, path: &str) -> Vec<(usize, usize)> {
        self.places
            .iter()
            .filter(|p| same_file(&p.path, path))
            .map(|p| (p.first, p.last))
            .collect()
    }

    /// Whether `symbol` is one of the names.
    #[must_use]
    pub fn names(&self, symbol: &str) -> bool {
        self.symbols.contains(symbol)
    }

    /// The names, in order.
    pub fn symbols(&self) -> impl Iterator<Item = &str> {
        self.symbols.iter().map(String::as_str)
    }
}
