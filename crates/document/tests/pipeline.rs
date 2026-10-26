// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The conversion pipeline against a scripted image-capable model.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use common::{build_pdf, FixturePage, Reply, ScriptedOcr, PROSE};
use sven_document::{
    manifest_path, ConvertOptions, Converter, DocumentError, HayroBackend, LayoutMode, Manifest,
    OcrMode,
};

fn converter(model: ScriptedOcr) -> (Converter, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::clone(&model.calls);
    (
        Converter::new(Arc::new(HayroBackend), vec![Arc::new(model)]),
        calls,
    )
}

fn options() -> ConvertOptions {
    ConvertOptions {
        model_spec: "scripted/ocr".into(),
        dpi: 40.0,
        max_attempts: 3,
        ..Default::default()
    }
}

/// Gray level identifies a scanned page; the scripted model answers with it.
fn page_of(gray: u8) -> String {
    format!("scanned page text {gray}")
}

fn write_pdf(dir: &std::path::Path, pages: &[FixturePage]) -> std::path::PathBuf {
    let path = dir.join("in.pdf");
    std::fs::write(&path, build_pdf(pages)).unwrap();
    path
}

fn mixed() -> Vec<FixturePage> {
    vec![
        FixturePage::Text(PROSE.to_vec()),
        FixturePage::Scan(100),
        FixturePage::Text(PROSE.to_vec()),
        FixturePage::Scan(120),
    ]
}

#[tokio::test]
async fn native_pages_skip_the_model_and_scans_are_recognised_in_page_order() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &mixed());
    let out = dir.path().join("out.md");
    // The later scan answers first.
    let (conv, calls) = converter(ScriptedOcr::new(|seen, _| Reply {
        delay: Duration::from_millis(if seen.gray == 100 { 150 } else { 0 }),
        ..Reply::text(page_of(seen.gray))
    }));
    let opts = ConvertOptions {
        page_markers: true,
        ..options()
    };
    let report = conv.convert(&src, &out, &opts).await.unwrap();

    assert_eq!(
        (report.pages_total, report.pages_native, report.pages_ocr),
        (4, 2, 2)
    );
    assert_eq!((report.pages_failed, report.pages_warning), (0, 0));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "native pages cost no request"
    );
    let md = std::fs::read_to_string(&out).unwrap();
    let at = |needle: &str| {
        md.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {md}"))
    };
    assert!(at("<!-- page: 1 -->") < at("committee") && at("committee") < at("<!-- page: 2 -->"));
    assert!(at("scanned page text 100") < at("<!-- page: 3 -->"));
    assert!(at("<!-- page: 3 -->") < at("scanned page text 120"));
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(manifest_path(&out)).unwrap()).unwrap();
    assert_eq!(manifest.pages.len(), 4);
    assert_eq!(
        manifest.output_sha256.as_deref(),
        Some(report.sha256.as_str())
    );
}

#[tokio::test]
async fn a_garbage_text_layer_is_not_trusted_and_ocr_always_ignores_a_good_one() {
    let dir = tempfile::tempdir().unwrap();
    let junk = vec![
        "tbe qvxkr; Hnpwq jjgfd !!lKj ssrtq mNbvc ,,zxcvb xqzzk wwrtt ptrqb",
        "fgdsa ;;lkjh nbvcx qwrtp zzxcv bnmkl ,mnbv cxzlk jhgfd sapoi uytrw",
    ];
    let src = write_pdf(
        dir.path(),
        &[FixturePage::Text(junk), FixturePage::Text(PROSE.to_vec())],
    );
    let out = dir.path().join("out.md");
    let (conv, calls) = converter(ScriptedOcr::new(|_, _| Reply::text("recognised")));
    let r = conv.convert(&src, &out, &options()).await.unwrap();
    assert_eq!((r.pages_ocr, r.pages_native), (1, 1));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let out2 = dir.path().join("all.md");
    let opts = ConvertOptions {
        ocr: OcrMode::Always,
        ..options()
    };
    let r = conv.convert(&src, &out2, &opts).await.unwrap();
    assert_eq!(r.pages_ocr, 2);
}

#[tokio::test]
async fn a_cutoff_page_is_retried_at_a_higher_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &[FixturePage::Scan(100)]);
    let out = dir.path().join("out.md");
    let widths = std::sync::Mutex::new(Vec::new());
    let widths = Arc::new(widths);
    let seen_widths = Arc::clone(&widths);
    let (conv, calls) = converter(ScriptedOcr::new(move |seen, call| {
        seen_widths.lock().unwrap().push(seen.width);
        if call == 0 {
            Reply {
                cutoff: true,
                ..Reply::text("half a pa")
            }
        } else {
            Reply::text("the whole page")
        }
    }));
    let r = conv.convert(&src, &out, &options()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!((r.pages_warning, r.pages_failed), (0, 0));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        "the whole page"
    );
    let w = widths.lock().unwrap().clone();
    assert!(w[1] > w[0], "retry renders larger: {w:?}");
}

#[tokio::test]
async fn a_page_that_never_validates_keeps_its_best_text_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(
        dir.path(),
        &[FixturePage::Scan(100), FixturePage::Scan(120)],
    );
    let out = dir.path().join("out.md");
    let (conv, _) = converter(ScriptedOcr::new(|seen, _| {
        if seen.gray == 100 {
            Reply {
                cutoff: true,
                ..Reply::text("always cut")
            }
        } else {
            Reply {
                error: Some("boom".into()),
                ..Reply::text("")
            }
        }
    }));
    let r = conv.convert(&src, &out, &options()).await.unwrap();
    assert_eq!((r.pages_warning, r.pages_failed), (1, 1));
    assert!(r
        .warnings
        .iter()
        .any(|w| w.contains("page 1") && w.contains("token limit")));
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(
        md.contains("always cut") && md.contains("page 2: conversion failed"),
        "{md}"
    );
}

#[tokio::test]
async fn a_rerun_reuses_accepted_pages_and_redoes_only_the_invalid_one() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &mixed());
    let out = dir.path().join("out.md");
    let (conv, calls) = converter(ScriptedOcr::new(|seen, _| Reply::text(page_of(seen.gray))));
    let first = conv.convert(&src, &out, &options()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let again = conv.convert(&src, &out, &options()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "nothing redone");
    assert_eq!(
        (again.pages_reused, again.sha256.clone()),
        (4, first.sha256.clone())
    );

    // Damage page 2 in the manifest: its text no longer matches its hash.
    let path = manifest_path(&out);
    let mut m: Manifest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    m.pages[1].text.push_str(" tampered");
    std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
    let healed = conv.convert(&src, &out, &options()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3, "only the damaged page");
    assert_eq!((healed.pages_reused, healed.sha256), (3, first.sha256));

    // A different prompt is different work.
    let opts = ConvertOptions {
        prompt: "Read it.".into(),
        ..options()
    };
    conv.convert(&src, &out, &opts).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn cancelling_leaves_a_consistent_manifest_and_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &mixed());
    let out = dir.path().join("out.md");
    let (conv, _) = converter(ScriptedOcr::new(|seen, _| Reply {
        // Page 2 answers at once; page 4 never does.
        delay: Duration::from_secs(if seen.gray == 120 { 3600 } else { 0 }),
        ..Reply::text(page_of(seen.gray))
    }));
    let opts = options();
    let run = conv.convert(&src, &out, &opts);
    assert!(tokio::time::timeout(Duration::from_millis(1500), run)
        .await
        .is_err());
    assert!(!out.exists(), "no output is published for a cancelled run");
    let m: Manifest = serde_json::from_slice(&std::fs::read(manifest_path(&out)).unwrap()).unwrap();
    assert!(m
        .pages
        .iter()
        .any(|p| p.page == 2 && p.text == page_of(100)));
    assert!(!m.pages.iter().any(|p| p.page == 4));
}

#[tokio::test]
async fn unusable_requests_fail_before_any_output_exists() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &mixed());
    let out = dir.path().join("out.md");
    let (conv, calls) = converter(ScriptedOcr::new(|_, _| Reply::text("x")).text_only());
    let err = conv.convert(&src, &out, &options()).await.unwrap_err();
    assert!(err.to_string().contains("image input"), "{err}");
    assert!(!out.exists() && !manifest_path(&out).exists());
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // `never` needs no model at all, so a text-only model is fine.
    let opts = ConvertOptions {
        ocr: OcrMode::Never,
        ..options()
    };
    let r = conv.convert(&src, &out, &opts).await.unwrap();
    assert_eq!(r.pages_native, 4);

    let bad = dir.path().join("bad.pdf");
    std::fs::write(&bad, b"not a pdf").unwrap();
    let err = conv.convert(&bad, &out, &options()).await.unwrap_err();
    assert!(matches!(err, DocumentError::InvalidPdf(_)), "{err}");
}

#[tokio::test]
async fn blank_pages_cost_no_request() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &[FixturePage::Blank, FixturePage::Scan(100)]);
    let out = dir.path().join("out.md");
    let (conv, calls) = converter(ScriptedOcr::new(|seen, _| Reply::text(page_of(seen.gray))));
    let r = conv.convert(&src, &out, &options()).await.unwrap();
    assert_eq!((r.pages_blank, r.pages_ocr), (1, 1));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn scans(n: u8) -> Vec<FixturePage> {
    (0..n).map(|i| FixturePage::Scan(60 + i * 10)).collect()
}

#[tokio::test]
async fn the_backend_stays_saturated_and_replicas_share_the_work() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &scans(12));
    let out = dir.path().join("out.md");
    let slow = |seen: &common::Seen, _| Reply {
        delay: Duration::from_millis(120),
        ..Reply::text(page_of(seen.gray))
    };
    let (a, b) = (ScriptedOcr::new(slow), ScriptedOcr::new(slow));
    let conv = Converter::new(Arc::new(HayroBackend), vec![Arc::new(a), Arc::new(b)]);
    let opts = ConvertOptions {
        concurrency: 3,
        ..options()
    };
    let r = conv.convert(&src, &out, &opts).await.unwrap();

    assert_eq!(r.pages_ocr, 12);
    assert_eq!(r.max_in_flight, 6, "3 slots on each of the 2 endpoints");
    assert_eq!(r.requests_per_endpoint.iter().sum::<u64>(), 12);
    assert!(
        r.requests_per_endpoint.iter().all(|n| *n >= 4),
        "{:?}",
        r.requests_per_endpoint
    );
    assert!(
        r.backend_idle_ms < 200,
        "the backend sat idle for {} ms while pages remained",
        r.backend_idle_ms
    );
}

#[tokio::test]
async fn pushback_from_the_server_is_waited_out_not_counted_as_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &scans(4));
    let out = dir.path().join("out.md");
    let (conv, calls) = converter(ScriptedOcr::new(|seen, call| {
        if call < 3 {
            Reply {
                error: Some("brain error 429 Too Many Requests".into()),
                ..Reply::text("")
            }
        } else {
            Reply::text(page_of(seen.gray))
        }
    }));
    let opts = ConvertOptions {
        concurrency: 4,
        max_attempts: 1,
        ..options()
    };
    let r = conv.convert(&src, &out, &opts).await.unwrap();
    assert_eq!((r.pages_ocr, r.pages_failed, r.pages_warning), (4, 0, 0));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        7,
        "three pushbacks, then every page once"
    );
}

/// A grounded answer in the order the model happened to list the blocks:
/// the right-hand column first.
const GROUNDED: &str = "<|ref|>text<|/ref|><|det|>[[500, 100, 900, 400]]<|/det|>\nright column (layout pass)\n\n\
                        <|ref|>title<|/ref|><|det|>[[100, 50, 300, 90]]<|/det|>\nHEADLINE\n\n\
                        <|ref|>text<|/ref|><|det|>[[100, 100, 400, 400]]<|/det|>\nleft column (layout pass)\n\n\
                        <|ref|>text<|/ref|><|det|>[[100, 420, 400, 500]]<|/det|>\nleft column, second block\n\n\
                        <|ref|>text<|/ref|><|det|>[[500, 420, 900, 500]]<|/det|>\nright column, second block\n";

/// Crops are told apart by width: the left column is narrower than the right.
fn layout_model() -> ScriptedOcr {
    ScriptedOcr::new(|seen, _| {
        if seen.prompt.contains("<|grounding|>") {
            return Reply::text(GROUNDED);
        }
        // Region crops at 72 dpi are 612 px x fraction of the page wide.
        match seen.width {
            w if w < 150 => Reply::text("Headline from its crop"),
            w if w < 230 => Reply::text("left column from its crop"),
            w if w < 300 => Reply::text("right column from its crop"),
            _ => Reply::text("plain whole-page reading"),
        }
    })
}

fn layout_options(layout: LayoutMode) -> ConvertOptions {
    ConvertOptions {
        layout,
        region_dpi: 72.0,
        dpi: 40.0,
        ..options()
    }
}

#[tokio::test]
async fn regions_are_read_from_crops_in_reading_order() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &[FixturePage::Scan(100)]);
    let out = dir.path().join("out.md");
    let (conv, calls) = converter(layout_model());
    let r = conv
        .convert(&src, &out, &layout_options(LayoutMode::Regions))
        .await
        .unwrap();
    assert_eq!(
        (r.pages_warning, r.pages_failed),
        (0, 0),
        "{:?}",
        r.warnings
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        6,
        "one layout pass and five blocks"
    );
    let md = std::fs::read_to_string(&out).unwrap();
    let at = |s: &str| md.find(s).unwrap_or_else(|| panic!("{s} in {md}"));
    assert!(at("# Headline from its crop") < at("left column from its crop"));
    assert!(at("left column from its crop") < at("right column from its crop"));
    assert!(
        !md.contains("layout pass"),
        "crops replace the layout pass's own text: {md}"
    );
    let m: Manifest = serde_json::from_slice(&std::fs::read(manifest_path(&out)).unwrap()).unwrap();
    let info = m.pages[0].layout.as_ref().expect("layout recorded");
    assert_eq!((info.regions, info.columns, info.degraded), (5, 2, 0));
}

#[tokio::test]
async fn a_model_that_does_not_ground_reads_the_page_whole_with_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &[FixturePage::Scan(100)]);
    let out = dir.path().join("out.md");
    let (conv, calls) = converter(ScriptedOcr::new(|_, _| {
        Reply::text("just markdown, no boxes")
    }));
    let r = conv
        .convert(&src, &out, &layout_options(LayoutMode::Regions))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!((r.pages_failed, r.pages_warning), (0, 0));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("does not locate blocks")),
        "{:?}",
        r.warnings
    );
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        "just markdown, no boxes"
    );
}

#[tokio::test]
async fn auto_falls_back_to_the_layout_pass_when_the_whole_page_reading_is_cut_off() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &[FixturePage::Scan(100)]);
    let out = dir.path().join("out.md");
    let model = ScriptedOcr::new(|seen, _| {
        if seen.prompt.contains("<|grounding|>") {
            Reply::text(GROUNDED)
        } else if seen.width > 300 {
            Reply {
                cutoff: true,
                ..Reply::text("column one column two column one")
            }
        } else if seen.width < 230 {
            Reply::text("left column from its crop")
        } else {
            Reply::text("right column from its crop")
        }
    });
    let (conv, _) = converter(model);
    let opts = ConvertOptions {
        max_attempts: 1,
        ..layout_options(LayoutMode::Auto)
    };
    let r = conv.convert(&src, &out, &opts).await.unwrap();
    assert_eq!(r.pages_failed, 0);
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(
        md.contains("left column from its crop") && !md.contains("column one"),
        "{md}"
    );
}

#[tokio::test]
async fn a_crop_the_model_invented_text_for_falls_back_to_the_layout_pass_text() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &[FixturePage::Scan(100)]);
    let out = dir.path().join("out.md");
    let model = ScriptedOcr::new(|seen, _| {
        if seen.prompt.contains("<|grounding|>") {
            Reply::text(GROUNDED)
        } else if (230..300).contains(&seen.width) {
            // The right column is a photograph; the model loops on it.
            Reply::text("same line again\n".repeat(12))
        } else {
            Reply::text("read from the crop")
        }
    });
    let (conv, _) = converter(model);
    let opts = ConvertOptions {
        max_attempts: 1,
        ..layout_options(LayoutMode::Regions)
    };
    let r = conv.convert(&src, &out, &opts).await.unwrap();
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(!md.contains("same line again"), "{md}");
    assert!(md.contains("right column (layout pass)"), "{md}");
    assert_eq!(r.pages_warning, 1);
    assert!(
        r.warnings.iter().any(|w| w.contains("2 of 5 blocks")),
        "{:?}",
        r.warnings
    );
}

#[tokio::test]
async fn a_line_on_the_seam_between_two_blocks_is_kept_once() {
    let dir = tempfile::tempdir().unwrap();
    let src = write_pdf(dir.path(), &[FixturePage::Scan(100)]);
    let out = dir.path().join("out.md");
    let model = ScriptedOcr::new(|seen, _| {
        if seen.prompt.contains("<|grounding|>") {
            Reply::text(GROUNDED)
        } else if seen.width < 150 {
            Reply::text("Headline")
        } else if seen.width < 230 {
            // The last line of the left column is also the top of the next block.
            Reply::text("first paragraph\nthe shared line on the seam")
        } else {
            Reply::text("the shared line on the seam\nrest of the right column")
        }
    });
    let (conv, _) = converter(model);
    conv.convert(&src, &out, &layout_options(LayoutMode::Regions))
        .await
        .unwrap();
    let md = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = md
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        lines.windows(2).all(|w| w[0] != w[1]),
        "a line on a seam appears twice in a row: {md}"
    );
}
