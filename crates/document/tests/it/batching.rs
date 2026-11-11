// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Several pages in one request: each page keeps its own text, a model that
//! does not keep them apart costs one request before the rest go page by
//! page, and a batch is chosen, never assumed, and fits the model's listed
//! output limit.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::common::{build_pdf, FixturePage, Reply, ScriptedOcr, Seen};
use sven_document::{
    batch_separator, manifest_path, ConvertOptions, Converter, HayroBackend, Manifest,
};

fn scans(grays: &[u8]) -> Vec<FixturePage> {
    grays.iter().map(|g| FixturePage::Scan(*g)).collect()
}

fn text_of(gray: u8) -> String {
    format!("scanned page text {gray}")
}

/// The pages a batched request names, in the order its images come.
fn pages_named(prompt: &str) -> Vec<usize> {
    let mut named: Vec<(usize, usize)> = (1..=64)
        .filter_map(|n| prompt.find(&batch_separator(n)).map(|at| (at, n)))
        .collect();
    named.sort_unstable();
    named.into_iter().map(|(_, n)| n).collect()
}

/// A model that does what the batched prompt asks.
fn follows_separators(seen: &Seen) -> String {
    let pages = pages_named(&seen.prompt);
    if pages.is_empty() {
        return text_of(seen.gray);
    }
    assert_eq!(pages.len(), seen.grays.len(), "one image per named page");
    pages
        .iter()
        .zip(&seen.grays)
        .map(|(page, gray)| format!("{}\n{}\n", batch_separator(*page), text_of(*gray)))
        .collect()
}

async fn convert(
    model: ScriptedOcr,
    grays: &[u8],
    pages_per_request: usize,
) -> (sven_document::Report, String, Manifest, usize) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("in.pdf");
    std::fs::write(&src, build_pdf(&scans(grays))).unwrap();
    let out = dir.path().join("out.md");
    let calls = Arc::clone(&model.calls);
    let conv = Converter::new(Arc::new(HayroBackend), vec![Arc::new(model)]);
    let options = ConvertOptions {
        model_spec: "scripted/ocr".into(),
        dpi: 40.0,
        pages_per_request,
        ..Default::default()
    };
    let report = conv.convert(&src, &out, &options).await.unwrap();
    let md = std::fs::read_to_string(&out).unwrap();
    let manifest = serde_json::from_slice(&std::fs::read(manifest_path(&out)).unwrap()).unwrap();
    (report, md, manifest, calls.load(Ordering::SeqCst))
}

fn assert_in_order(md: &str, grays: &[u8]) {
    let mut from = 0;
    for gray in grays {
        let at = md[from..]
            .find(&text_of(*gray))
            .unwrap_or_else(|| panic!("{} after byte {from} in {md}", text_of(*gray)));
        from += at;
    }
    assert!(!md.contains("=== PAGE"), "separators are not output: {md}");
}

#[tokio::test]
async fn several_pages_go_in_one_request_and_each_keeps_its_own_text() {
    let grays = [100, 110, 120, 130, 140];
    let model = ScriptedOcr::new(|seen, _| Reply::text(follows_separators(seen)));
    let (report, md, manifest, calls) = convert(model, &grays, 2).await;
    assert_eq!(calls, 3, "five pages two at a time");
    assert_eq!(
        (report.pages_ocr, report.pages_warning, report.pages_failed),
        (5, 0, 0)
    );
    assert_eq!(report.pages_batched, 4, "the odd page out is read alone");
    assert_in_order(&md, &grays);
    for page in &manifest.pages {
        assert_eq!(page.text, text_of(grays[page.page - 1]));
    }
}

#[tokio::test]
async fn a_model_that_does_not_keep_pages_apart_costs_one_request_then_pages_go_one_by_one() {
    let grays = [100, 110, 120, 130, 140, 150];
    // Reads only the first image of whatever it is sent.
    let model = ScriptedOcr::new(|seen, _| Reply::text(text_of(seen.gray)));
    let (report, md, _, calls) = convert(model, &grays, 3).await;
    assert_eq!(
        calls,
        1 + 6,
        "one batch to learn it, then a request per page"
    );
    assert_eq!((report.pages_ocr, report.pages_failed), (6, 0));
    assert_eq!(report.pages_batched, 0);
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("one page per request")),
        "{:?}",
        report.notes
    );
    assert_in_order(&md, &grays);
}

#[tokio::test]
async fn pages_go_alone_unless_chosen_and_a_batch_never_outgrows_the_listed_output_limit() {
    let grays = [100, 110, 120, 130];
    let model = || {
        ScriptedOcr::new(|seen, _| Reply::text(follows_separators(seen))).listing_max_output(8192)
    };
    let (_, _, _, calls) = convert(model(), &grays, 1).await;
    assert_eq!(calls, 4, "the default reads every page alone");
    // Four pages asked for, but 4,096 tokens a page fit only twice in 8,192.
    let (_, md, _, calls) = convert(model(), &grays, 4).await;
    assert_eq!(calls, 2);
    assert_in_order(&md, &grays);
    assert_eq!(ConvertOptions::default().pages_per_request, 1);
}

#[tokio::test]
async fn a_page_of_a_batch_that_does_not_validate_is_read_again_on_its_own() {
    let grays = [100, 110];
    // The batch answer leaves the second page empty; read alone it is fine.
    let model = ScriptedOcr::new(|seen, _| {
        let pages = pages_named(&seen.prompt);
        if pages.is_empty() {
            return Reply::text(text_of(seen.gray));
        }
        Reply::text(format!(
            "{}\n{}\n{}\n",
            batch_separator(pages[0]),
            text_of(seen.grays[0]),
            batch_separator(pages[1]),
        ))
    });
    let (report, md, _, calls) = convert(model, &grays, 2).await;
    assert_eq!(calls, 2, "the batch, then the empty page alone");
    assert_eq!((report.pages_ocr, report.pages_warning), (2, 0));
    assert_eq!(report.pages_batched, 1);
    assert_in_order(&md, &grays);
}
