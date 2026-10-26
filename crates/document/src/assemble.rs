// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Page texts to one Markdown document, in page order.

use std::collections::HashMap;

use crate::manifest::{PageRecord, Status};

#[derive(Debug, Clone, Copy, Default)]
pub struct AssembleOptions {
    /// Put `<!-- page: N -->` before each page.
    pub page_markers: bool,
    /// Remove a first or last line that repeats on most pages.
    pub dedupe_headers: bool,
}

/// A line must lead or trail at least this share of the pages (and at least
/// [`MIN_PAGES_FOR_DEDUPE`] of them) to count as a running header or footer.
const REPEAT_SHARE: f32 = 0.6;
const MIN_PAGES_FOR_DEDUPE: usize = 4;

pub fn assemble(pages: &[PageRecord], options: AssembleOptions) -> String {
    let mut texts: Vec<String> = pages
        .iter()
        .map(|p| match p.status {
            Status::Failed => format!("<!-- page {}: conversion failed -->", p.page),
            _ => p.text.trim().to_string(),
        })
        .collect();
    if options.dedupe_headers {
        strip_running_lines(&mut texts);
    }
    let mut out = String::new();
    for (page, text) in pages.iter().zip(&texts) {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        if options.page_markers {
            out.push_str(&format!("<!-- page: {} -->\n\n", page.page));
        }
        out.push_str(text);
    }
    out.push('\n');
    out
}

fn strip_running_lines(texts: &mut [String]) {
    if texts.len() < MIN_PAGES_FOR_DEDUPE {
        return;
    }
    let needed = ((texts.len() as f32) * REPEAT_SHARE).ceil() as usize;
    for from_end in [false, true] {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for text in texts.iter() {
            if let Some(line) = edge_line(text, from_end) {
                *counts.entry(line).or_default() += 1;
            }
        }
        for text in texts.iter_mut() {
            let Some(line) = edge_line(text, from_end) else {
                continue;
            };
            if counts.get(&line).copied().unwrap_or(0) >= needed {
                *text = remove_edge(text, from_end);
            }
        }
    }
}

/// The first or last non-empty line, if short enough to be a header.
fn edge_line(text: &str, from_end: bool) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = if from_end {
        lines.next_back()
    } else {
        lines.next()
    }?;
    (line.chars().count() <= 100).then(|| line.to_string())
}

fn remove_edge(text: &str, from_end: bool) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    let position = if from_end {
        lines.iter().rposition(|l| !l.trim().is_empty())
    } else {
        lines.iter().position(|l| !l.trim().is_empty())
    };
    if let Some(i) = position {
        lines.remove(i);
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Method;

    fn page(n: usize, text: &str, status: Status) -> PageRecord {
        PageRecord {
            page: n,
            key: String::new(),
            method: Method::Ocr,
            status,
            model: None,
            attempts: 1,
            dpi: None,
            sha256: String::new(),
            text: text.into(),
            warnings: vec![],
            layout: None,
            render_ms: 0,
            ocr_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    #[test]
    fn pages_in_order_with_markers_and_failure_placeholder() {
        let pages = vec![page(2, "two", Status::Ok), page(3, "", Status::Failed)];
        let md = assemble(
            &pages,
            AssembleOptions {
                page_markers: true,
                dedupe_headers: false,
            },
        );
        assert_eq!(
            md,
            "<!-- page: 2 -->\n\ntwo\n\n<!-- page: 3 -->\n\n<!-- page 3: conversion failed -->\n"
        );
    }

    #[test]
    fn running_header_is_removed_only_when_asked_and_widespread() {
        let pages: Vec<_> = (1..=5)
            .map(|n| page(n, &format!("ACME Quarterly\nbody {n}\n{n}"), Status::Ok))
            .collect();
        let kept = assemble(&pages, AssembleOptions::default());
        assert!(kept.contains("ACME Quarterly"));
        let md = assemble(
            &pages,
            AssembleOptions {
                page_markers: false,
                dedupe_headers: true,
            },
        );
        assert!(!md.contains("ACME Quarterly"));
        assert!(md.contains("body 3"), "body text is never a running line");
    }
}
