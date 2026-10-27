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

/// Outline entries whose title contains `query` (case and spacing ignored).
pub fn outline_matches<'a>(entries: &'a [OutlineEntry], query: &str) -> Vec<&'a OutlineEntry> {
    let needle = normalize(query);
    entries
        .iter()
        .filter(|e| normalize(&e.title).contains(&needle))
        .collect()
}

/// A page whose text contains the query, with the text around the first hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHit {
    /// 1-based page.
    pub page: usize,
    pub snippet: String,
}

/// Pages of `native_text` (one entry per page) containing `query`, case and
/// spacing ignored, at most `limit`, with the total number of matching pages.
pub fn find(native_text: &[Option<String>], query: &str, limit: usize) -> (Vec<PageHit>, usize) {
    let needle = normalize(query);
    let mut hits = Vec::new();
    let mut total = 0;
    for (i, text) in native_text.iter().enumerate() {
        let Some(text) = text else { continue };
        let flat = normalize(text);
        let Some(at) = flat.find(&needle) else {
            continue;
        };
        total += 1;
        if hits.len() < limit {
            let start = flat[..at]
                .char_indices()
                .rev()
                .nth(60)
                .map_or(0, |(i, _)| i);
            let end = flat[at..]
                .char_indices()
                .nth(needle.len() + 60)
                .map_or(flat.len(), |(i, _)| at + i);
            hits.push(PageHit {
                page: i + 1,
                snippet: flat[start..end].to_string(),
            });
        }
    }
    (hits, total)
}

/// Lower case, every run of whitespace one space.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let hits = outline_matches(&entries, "usart");
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].first_page, hits[0].last_page), (3, 4));
    }

    #[test]
    fn a_pdf_without_bookmarks_has_an_empty_outline() {
        let plain = crate::testing::build_pdf(&[FixturePage::Text(vec!["x"])]);
        assert!(outline(&plain).unwrap().is_empty());
    }

    #[test]
    fn text_search_reports_pages_and_a_snippet() {
        let text = [
            Some("Intro".to_string()),
            None,
            Some("The USART  Status register (USART_SR)".to_string()),
        ];
        let (hits, total) = find(&text, "usart_sr", 10);
        assert_eq!(total, 1);
        assert_eq!(hits[0].page, 3);
        assert!(hits[0].snippet.contains("(usart_sr)"));
    }
}
