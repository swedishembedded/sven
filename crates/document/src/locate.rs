// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Finding where something is in a PDF without reading it: the outline
//! (bookmarks with their page ranges) and a search of the native text layer.
//!
//! An agent asked for "the USART chapter of the manual" locates the pages
//! here, then converts just those pages; no page text passes through the chat
//! model on the way.
//!
//! Swedish Embedded AB implements agent tools that move bulk data around the
//! language model instead of through it. If your team needs expertise in
//! building cost-efficient document agents then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::query::{normalize, Hit, Query};
use crate::DocumentError;

/// Outline entries as `(level, title, first page)`, and the page count.
type RawOutline = (Vec<(usize, String, usize)>, usize);

/// One bookmark of the outline, with the pages it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    /// 1 for a top-level entry, 2 for its children, ...
    pub level: usize,
    pub title: String,
    /// 1-based first page.
    pub first_page: usize,
    /// 1-based last page: the page before the next entry at this level or
    /// above, or the document's last page.
    pub last_page: usize,
}

/// The document outline in reading order, empty when the PDF has none.
pub fn outline(data: &[u8]) -> Result<Vec<OutlineEntry>, DocumentError> {
    let parsed = catch_unwind(AssertUnwindSafe(|| -> Result<RawOutline, String> {
        let doc = lopdf::Document::load_mem(data).map_err(|e| e.to_string())?;
        let pages = doc.get_pages().len();
        // No /Outlines in the catalog is a document without bookmarks, not an error.
        let has_outline = doc
            .catalog()
            .map(|c| c.has(b"Outlines"))
            .map_err(|e| e.to_string())?;
        let toc = if has_outline {
            let toc = doc.get_toc().map_err(|e| e.to_string())?;
            toc.toc
                .into_iter()
                .map(|t| (t.level, t.title, t.page))
                .collect()
        } else {
            Vec::new()
        };
        Ok((toc, pages))
    }));
    let (toc, pages) = match parsed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            return Err(DocumentError::InvalidPdf(format!(
                "reading the outline: {e}"
            )))
        }
        Err(_) => {
            return Err(DocumentError::InvalidPdf(
                "reading the outline panicked".into(),
            ))
        }
    };
    Ok(with_ranges(toc, pages))
}

fn with_ranges(toc: Vec<(usize, String, usize)>, pages: usize) -> Vec<OutlineEntry> {
    let starts: Vec<(usize, usize)> = toc.iter().map(|(level, _, page)| (*level, *page)).collect();
    toc.into_iter()
        .enumerate()
        .map(|(i, (level, title, page))| {
            let next = starts[i + 1..]
                .iter()
                .find(|(l, _)| *l <= level)
                .map(|(_, p)| *p);
            let last_page = next.map_or(pages, |p| p.saturating_sub(1).max(page));
            OutlineEntry {
                level,
                title: title.trim().to_string(),
                first_page: page,
                last_page,
            }
        })
        .collect()
}

/// Outline entries whose title matches `query`, with the match: in reading
/// order, or best first for a query that scores (fzf).
pub fn outline_matches<'a>(
    entries: &'a [OutlineEntry],
    query: &Query,
) -> Vec<(&'a OutlineEntry, Hit)> {
    let mut found: Vec<_> = entries
        .iter()
        .filter_map(|e| query.best(&normalize(&e.title)).map(|h| (e, h)))
        .collect();
    // Stable: equal scores keep reading order.
    found.sort_by_key(|(_, h)| h.score);
    found
}

/// A page that matches a query, with the text around its best match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHit {
    /// 1-based page.
    pub page: usize,
    /// Matches on the page: occurrences for a literal or regex query,
    /// matching lines for an fzf one.
    pub count: usize,
    /// The best match with up to `context` characters on each side, its
    /// whitespace collapsed.
    pub snippet: String,
    /// The best match's score; lower is better.
    pub score: usize,
}

/// The pages among `pages` (0-based indices into `native_text`, one entry
/// per page) that match `query`, in page order, or best first for a query
/// that scores (fzf).
pub fn find(
    native_text: &[Option<String>],
    query: &Query,
    pages: &[usize],
    context: usize,
) -> Vec<PageHit> {
    let mut hits: Vec<PageHit> = pages
        .iter()
        .filter_map(|&i| {
            let text = native_text.get(i)?.as_deref()?;
            page_hit(text, query, context).map(|(count, snippet, score)| PageHit {
                page: i + 1,
                count,
                snippet,
                score,
            })
        })
        .collect();
    hits.sort_by_key(|h| h.score);
    hits
}

/// The match count, snippet and score of `text`, if it matches.
fn page_hit(text: &str, query: &Query, context: usize) -> Option<(usize, String, usize)> {
    if !query.by_line() {
        let flat = normalize(text);
        let hit = query.best(&flat)?;
        return Some((query.count(&flat), snippet(&flat, &hit, context), hit.score));
    }
    let mut count = 0;
    let mut best: Option<(String, Hit)> = None;
    for line in text.lines() {
        let flat = normalize(line);
        let Some(hit) = query.best(&flat) else {
            continue;
        };
        count += 1;
        if best.as_ref().is_none_or(|(_, b)| hit.score < b.score) {
            best = Some((flat, hit));
        }
    }
    let (line, hit) = best?;
    Some((count, snippet(&line, &hit, context), hit.score))
}

/// The match with up to `context` characters on each side.
fn snippet(text: &str, hit: &Hit, context: usize) -> String {
    let start = text[..hit.span.start]
        .char_indices()
        .rev()
        .nth(context.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    let end = text[hit.span.end..]
        .char_indices()
        .nth(context)
        .map_or(text.len(), |(i, _)| hit.span.end + i);
    let start = if context == 0 { hit.span.start } else { start };
    text[start..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Syntax;
    use crate::testing::{build_pdf_with_outline, FixturePage};

    fn manual() -> Vec<u8> {
        let page = |t: &'static str| FixturePage::Text(vec![t]);
        build_pdf_with_outline(
            &[
                page("Contents"),
                page("Introduction"),
                page("USART overview"),
                page("USART registers"),
                page("SPI overview"),
                page("SPI registers"),
            ],
            &[
                (1, "1 Introduction", 1),
                (
                    1,
                    "2 Universal synchronous asynchronous receiver transmitter (USART)",
                    2,
                ),
                (2, "2.1 USART registers", 3),
                (1, "3 Serial peripheral interface (SPI)", 4),
            ],
        )
    }

    #[test]
    fn outline_entries_carry_the_pages_they_cover() {
        let entries = outline(&manual()).unwrap();
        let ranges: Vec<_> = entries
            .iter()
            .map(|e| (e.level, e.first_page, e.last_page))
            .collect();
        assert_eq!(ranges, [(1, 2, 2), (1, 3, 4), (2, 4, 4), (1, 5, 6)]);
    }

    #[test]
    fn a_chapter_is_found_by_part_of_its_title() {
        let entries = outline(&manual()).unwrap();
        let hits = outline_matches(&entries, &Query::new("usart", Syntax::Literal).unwrap());
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].0.first_page, hits[0].0.last_page), (3, 4));
    }

    #[test]
    fn a_pdf_without_bookmarks_has_an_empty_outline() {
        let plain = crate::testing::build_pdf(&[FixturePage::Text(vec!["x"])]);
        assert!(outline(&plain).unwrap().is_empty());
    }

    #[test]
    fn text_search_reports_pages_counts_and_a_snippet_in_the_documents_spelling() {
        let text = [
            Some("Intro".to_string()),
            None,
            Some("The USART  Status register (USART_SR)\nUSART_SR again".to_string()),
        ];
        let query = Query::new("usart_sr", Syntax::Literal).unwrap();
        let hits = find(&text, &query, &[0, 1, 2], 10);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].page, hits[0].count), (3, 2));
        assert_eq!(hits[0].snippet, "register (USART_SR) USART_SR");
        assert!(
            find(&text, &query, &[0, 1], 10).is_empty(),
            "only the selected pages"
        );
    }

    #[test]
    fn an_fzf_search_ranks_pages_by_their_best_line() {
        let text = [
            Some("bus access unit disable".to_string()),
            Some("Baud rate register\nother".to_string()),
        ];
        let query = Query::new("baud", Syntax::Fzf).unwrap();
        let hits = find(&text, &query, &[0, 1], 40);
        assert_eq!(hits.iter().map(|h| h.page).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(hits[0].snippet, "Baud rate register");
    }
}
