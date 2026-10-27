// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Search queries for document text: literal, regular expression, or fzf's
//! extended search syntax.
//!
//! Every syntax ignores case and treats any run of whitespace as one space,
//! because a PDF's text layer breaks lines and spaces wherever the layout put
//! them. A literal or regex query runs over a page's whole text, so a phrase
//! wrapped across lines still matches; an fzf query is line-oriented, as in
//! fzf, and runs over each line on its own.
//!
//! Swedish Embedded AB implements agent tools that find what matters in large
//! technical documents for its clients. If your team needs expertise in
//! search over reference manuals then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use crate::DocumentError;

/// How a query's text is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Syntax {
    /// The text itself.
    #[default]
    Literal,
    /// A regular expression (Rust `regex` syntax; `(?-i)` makes it
    /// case-sensitive).
    Regex,
    /// fzf's extended search: space-separated terms that must all match a
    /// line; `abc` fuzzy, `'abc` exact, `^abc` prefix, `abc$` suffix, `!abc`
    /// excludes, `a | b` either; `\ ` is a literal space.
    Fzf,
}

impl Syntax {
    /// Every syntax's name, as [`Syntax::from_name`] takes it.
    pub const NAMES: [&'static str; 3] = ["literal", "regex", "fzf"];

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "literal" => Some(Self::Literal),
            "regex" => Some(Self::Regex),
            "fzf" => Some(Self::Fzf),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Regex => "regex",
            Self::Fzf => "fzf",
        }
    }
}

/// Where a query matched one text, and how well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The byte range of the match in the normalized text.
    pub span: Range<usize>,
    /// 0 is an exact match; a fuzzy match scores the characters it skipped.
    /// Lower is better.
    pub score: usize,
}

/// A compiled query.
#[derive(Debug, Clone)]
pub struct Query {
    syntax: Syntax,
    matcher: Matcher,
}

#[derive(Debug, Clone)]
enum Matcher {
    /// Literal queries compile to an escaped regex: one engine, one notion of
    /// case folding.
    Regex(Regex),
    /// Groups that must all match; a group matches when any of its terms does.
    Fzf(Vec<Vec<Term>>),
}

#[derive(Debug, Clone)]
struct Term {
    /// Case-folded characters.
    text: Vec<char>,
    kind: TermKind,
    negated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TermKind {
    Fuzzy,
    Exact,
    Prefix,
    Suffix,
    Line,
}

impl Query {
    /// Compiles `text` read as `syntax`. A query that is empty, does not
    /// parse, or matches the empty string (and so every position of every
    /// page) is refused.
    pub fn new(text: &str, syntax: Syntax) -> Result<Self, DocumentError> {
        let bad = |why: String| DocumentError::Query(text.to_string(), why);
        let flat = normalize(text);
        if flat.is_empty() {
            return Err(bad("the query is empty".into()));
        }
        let matcher = match syntax {
            Syntax::Literal => Matcher::Regex(regex(&regex::escape(&flat)).map_err(bad)?),
            Syntax::Regex => {
                let re = regex(text.trim()).map_err(bad)?;
                if re.is_match("") {
                    return Err(bad("the pattern matches the empty string".into()));
                }
                Matcher::Regex(re)
            }
            Syntax::Fzf => Matcher::Fzf(fzf_groups(text).map_err(bad)?),
        };
        Ok(Self { syntax, matcher })
    }

    #[must_use]
    pub fn syntax(&self) -> Syntax {
        self.syntax
    }

    /// `true` for a query that runs over a page's text line by line.
    #[must_use]
    pub fn by_line(&self) -> bool {
        matches!(self.matcher, Matcher::Fzf(_))
    }

    /// The best match in `text`, which the caller has passed through
    /// [`normalize`].
    #[must_use]
    pub fn best(&self, text: &str) -> Option<Hit> {
        match &self.matcher {
            Matcher::Regex(re) => re.find(text).map(|m| Hit {
                span: m.range(),
                score: 0,
            }),
            Matcher::Fzf(groups) => fzf_match(groups, text),
        }
    }

    /// How many times the query matches `text` (normalized): non-overlapping
    /// matches for a literal or regex query, 0 or 1 for an fzf one.
    #[must_use]
    pub fn count(&self, text: &str) -> usize {
        match &self.matcher {
            Matcher::Regex(re) => re.find_iter(text).count(),
            Matcher::Fzf(groups) => usize::from(fzf_match(groups, text).is_some()),
        }
    }
}

/// Every run of whitespace one space, trimmed. Case is kept: the query folds
/// it, so snippets show the document's own spelling.
#[must_use]
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn regex(pattern: &str) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|e| e.to_string())
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// The query split into its OR-groups of terms.
fn fzf_groups(text: &str) -> Result<Vec<Vec<Term>>, String> {
    let mut groups: Vec<Vec<Term>> = Vec::new();
    let mut or_next = false;
    for word in fzf_words(text) {
        if word == "|" {
            if groups.is_empty() || or_next {
                return Err("`|` needs a term on each side".into());
            }
            or_next = true;
            continue;
        }
        let term = fzf_term(&word)?;
        match groups.last_mut() {
            Some(group) if or_next => group.push(term),
            _ => groups.push(vec![term]),
        }
        or_next = false;
    }
    if or_next {
        return Err("`|` needs a term on each side".into());
    }
    if groups.iter().all(|g| g.iter().all(|t| t.negated)) {
        // fzf matches every line then; a search that returns every page is
        // not what anyone asked for.
        return Err("the query only excludes; add a term to look for".into());
    }
    Ok(groups)
}

/// Whitespace-separated words, with `\ ` kept as a space inside a word.
fn fzf_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&' ') {
            word.push(' ');
            chars.next();
        } else if c.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn fzf_term(word: &str) -> Result<Term, String> {
    let (negated, rest) = match word.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, word),
    };
    let (kind, text) = if let Some(t) = rest.strip_prefix('\'') {
        (TermKind::Exact, t)
    } else if let Some(t) = rest.strip_prefix('^') {
        match t.strip_suffix('$') {
            Some(t) => (TermKind::Line, t),
            None => (TermKind::Prefix, t),
        }
    } else if let Some(t) = rest.strip_suffix('$') {
        (TermKind::Suffix, t)
    } else if negated {
        // As in fzf: a negated plain term is an exact one; "lines without
        // these letters somewhere in order" excludes almost everything.
        (TermKind::Exact, rest)
    } else {
        (TermKind::Fuzzy, rest)
    };
    if text.is_empty() {
        return Err(format!("`{word}` has no text to match"));
    }
    Ok(Term {
        text: text.chars().map(fold).collect(),
        kind,
        negated,
    })
}

/// The match of every group in `line`: the span from the first matched
/// character to the last, and the summed score; `None` when a group fails.
fn fzf_match(groups: &[Vec<Term>], line: &str) -> Option<Hit> {
    let chars: Vec<(usize, char)> = line.char_indices().map(|(i, c)| (i, fold(c))).collect();
    let folded: Vec<char> = chars.iter().map(|(_, c)| *c).collect();
    let mut span: Option<Range<usize>> = None;
    let mut score = 0;
    for group in groups {
        let mut best: Option<(Range<usize>, usize)> = None;
        let mut group_matches = false;
        for term in group {
            let found = term_match(term, &folded);
            if term.negated {
                group_matches |= found.is_none();
                continue;
            }
            if let Some(m) = found {
                group_matches = true;
                if best.as_ref().is_none_or(|(_, s)| m.1 < *s) {
                    best = Some(m);
                }
            }
        }
        if !group_matches {
            return None;
        }
        if let Some((r, s)) = best {
            score += s;
            span = Some(match span {
                Some(old) => old.start.min(r.start)..old.end.max(r.end),
                None => r,
            });
        }
    }
    // Char indices to byte offsets.
    let span = span?;
    let byte = |i: usize| chars.get(i).map_or(line.len(), |(b, _)| *b);
    Some(Hit {
        span: byte(span.start)..byte(span.end),
        score,
    })
}

/// A term's match in `line` as a char range and its score.
fn term_match(term: &Term, line: &[char]) -> Option<(Range<usize>, usize)> {
    let t = &term.text;
    let n = t.len();
    let exact = |at: usize| (at..at + n, 0);
    match term.kind {
        TermKind::Exact => line.windows(n).position(|w| w == t.as_slice()).map(exact),
        TermKind::Prefix => line.starts_with(t).then(|| exact(0)),
        TermKind::Suffix => line.ends_with(t).then(|| exact(line.len() - n)),
        TermKind::Line => (line == t.as_slice()).then(|| exact(0)),
        TermKind::Fuzzy => fuzzy(t, line),
    }
}

/// fzf's first algorithm: the first end of `pattern` as a subsequence of
/// `line`, then the latest start before that end, so the span is tight.
/// Scores the characters inside the span that are not the pattern's.
fn fuzzy(pattern: &[char], line: &[char]) -> Option<(Range<usize>, usize)> {
    let mut p = 0;
    let mut end = None;
    for (i, c) in line.iter().enumerate() {
        if *c == pattern[p] {
            p += 1;
            if p == pattern.len() {
                end = Some(i + 1);
                break;
            }
        }
    }
    let end = end?;
    let mut p = pattern.len();
    let mut start = end;
    while p > 0 {
        start -= 1;
        if line[start] == pattern[p - 1] {
            p -= 1;
        }
    }
    Some((start..end, end - start - pattern.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(query: &str, syntax: Syntax, text: &str) -> Option<Hit> {
        Query::new(query, syntax).unwrap().best(&normalize(text))
    }

    #[test]
    fn a_literal_query_ignores_case_and_line_breaks() {
        let text = "the USART\n  status   register";
        let h = hit("usart status register", Syntax::Literal, text).unwrap();
        assert_eq!(&normalize(text)[h.span], "USART status register");
        // Regex metacharacters in a literal query are just characters.
        assert!(hit("USART_SR (0x00)", Syntax::Literal, "USART_SR (0x00) reset").is_some());
    }

    #[test]
    fn a_regex_query_matches_register_families() {
        let q = Query::new(r"USART_(SR|DR)\b", Syntax::Regex).unwrap();
        assert_eq!(q.count("usart_sr, USART_DR and USART_BRR"), 2);
        assert!(Query::new("(?-i)usart_sr", Syntax::Regex)
            .unwrap()
            .best("USART_SR")
            .is_none());
    }

    #[test]
    fn queries_that_match_everything_or_do_not_parse_are_refused() {
        for (q, syntax) in [
            ("a*", Syntax::Regex),
            ("(", Syntax::Regex),
            ("  ", Syntax::Literal),
            ("!usart", Syntax::Fzf),
            ("usart |", Syntax::Fzf),
            ("^", Syntax::Fzf),
        ] {
            assert!(
                matches!(Query::new(q, syntax), Err(DocumentError::Query(..))),
                "{q:?} as {syntax:?}"
            );
        }
    }

    #[test]
    fn fzf_terms_follow_fzf_extended_search() {
        let line = "25.6.1 Status register (USART_SR)";
        let fzf = |q: &str| hit(q, Syntax::Fzf, line).is_some();
        assert!(fzf("stsreg"), "fuzzy subsequence");
        assert!(
            fzf("'status ^25.6 _sr)$"),
            "exact, prefix and suffix together"
        );
        assert!(!fzf("'stsreg"), "exact is not fuzzy");
        assert!(!fzf("status !usart"), "an excluded term");
        assert!(fzf("spi | usart"), "either side of |");
        assert!(fzf(r"status\ register"), "an escaped space");
        assert!(!fzf("^status"), "prefix anchors at the line start");
    }

    #[test]
    fn a_tight_fuzzy_match_scores_better_than_a_loose_one() {
        let tight = hit("baud", Syntax::Fzf, "baud rate register").unwrap();
        let loose = hit("baud", Syntax::Fzf, "bus access unit disable").unwrap();
        assert!(tight.score < loose.score);
        assert_eq!(tight.span, 0..4);
    }
}
