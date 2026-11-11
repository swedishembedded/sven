// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What can be converted: a PDF, or one picture of a page (a photo or a scan
//! as PNG, JPEG, WebP, GIF or BMP), which is a document of one page.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::common::{build_pdf, FixturePage, Reply, ScriptedOcr, PROSE};
use sven_document::{ConvertOptions, Converter, DocumentError, StandardBackend};

fn picture(gray: u8, format: image::ImageFormat) -> Vec<u8> {
    // A tone with dark lines across it, as text crosses a page; the centre
    // pixel, the picture's identity here, is the tone.
    let img = image::GrayImage::from_fn(300, 400, |_, y| {
        image::Luma([if y % 40 == 7 { 0 } else { gray }])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut out, format)
        .unwrap();
    out.into_inner()
}

fn picture_options() -> ConvertOptions {
    ConvertOptions {
        model_spec: "scripted/ocr".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_picture_of_a_page_is_a_document_of_one_page_read_by_the_model() {
    let dir = tempfile::tempdir().unwrap();
    for (name, format, gray) in [
        ("receipt.png", image::ImageFormat::Png, 100),
        ("receipt.jpg", image::ImageFormat::Jpeg, 120),
    ] {
        let src = dir.path().join(name);
        std::fs::write(&src, picture(gray, format)).unwrap();
        let model = ScriptedOcr::new(|seen, _| Reply::text(format!("total {}", seen.gray / 10)));
        let calls = Arc::clone(&model.calls);
        let conv = Converter::new(Arc::new(StandardBackend), vec![Arc::new(model)]);
        let out = dir.path().join(format!("{name}.md"));
        let report = conv.convert(&src, &out, &picture_options()).await.unwrap();
        assert_eq!((report.pages_total, report.pages_ocr), (1, 1), "{name}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{name}");
        // JPEG is lossy: the gray level survives to within a few steps.
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text.trim(), format!("total {}", gray / 10), "{name}");
    }
}

#[tokio::test]
async fn a_pdf_is_still_read_as_a_pdf() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("scan.pdf");
    std::fs::write(
        &src,
        build_pdf(&[FixturePage::Scan(140), FixturePage::Text(PROSE.to_vec())]),
    )
    .unwrap();
    let model = ScriptedOcr::new(|_, _| Reply::text("recognised"));
    let conv = Converter::new(Arc::new(StandardBackend), vec![Arc::new(model)]);
    let report = conv
        .convert(&src, &dir.path().join("out.md"), &picture_options())
        .await
        .unwrap();
    assert_eq!(
        (report.pages_total, report.pages_ocr, report.pages_native),
        (2, 1, 1)
    );
}

#[tokio::test]
async fn a_file_that_is_neither_a_pdf_nor_a_picture_is_refused_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("notes.txt");
    std::fs::write(&src, "just some words").unwrap();
    let model = ScriptedOcr::new(|_, _| Reply::text("x"));
    let calls = Arc::clone(&model.calls);
    let conv = Converter::new(Arc::new(StandardBackend), vec![Arc::new(model)]);
    let err = conv
        .convert(&src, &dir.path().join("out.md"), &picture_options())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DocumentError::Unsupported(_)),
        "unexpected: {err}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!dir.path().join("out.md").exists());
}
