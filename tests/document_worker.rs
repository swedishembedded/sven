// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a document read by `sven document worker` processes, confined as a
//! hosted host confines them (no network, nothing writable, limits of their
//! own), reads exactly as it does in process.
#![cfg(all(feature = "document", target_os = "linux"))]

use std::path::PathBuf;
use std::sync::Arc;

use sven_bootstrap::document_worker::ProcessWorkers;
use sven_document::testing::{build_pdf_with_outline, FixturePage, PROSE};
use sven_document::{DocumentBackend, HayroBackend, IsolatedBackend, WorkerLimits};

#[test]
fn a_document_reads_the_same_in_confined_worker_processes() {
    let pdf = Arc::new(build_pdf_with_outline(
        &[FixturePage::Text(PROSE.to_vec()), FixturePage::Scan(100)],
        &[(1, "Scans", 1)],
    ));
    let workers = ProcessWorkers::new(PathBuf::from(env!("CARGO_BIN_EXE_sven")), 1024, 60);
    let isolated = IsolatedBackend::new(Arc::new(workers), WorkerLimits::default())
        .open(Arc::clone(&pdf))
        .expect("a confined worker opens the document");
    let local = HayroBackend.open(pdf).unwrap();
    assert_eq!(isolated.page_count(), 2);
    assert_eq!(isolated.native_text(), local.native_text());
    assert_eq!(isolated.outline().unwrap(), local.outline().unwrap());
    let page = isolated.render(1, 40.0).unwrap();
    assert!(page.png.starts_with(&[0x89, b'P', b'N', b'G']));
    assert_eq!((page.width, page.height), {
        let l = local.render(1, 40.0).unwrap();
        (l.width, l.height)
    });
    // What is not a document is refused by the worker, as in process.
    let refused = IsolatedBackend::new(
        Arc::new(ProcessWorkers::new(
            PathBuf::from(env!("CARGO_BIN_EXE_sven")),
            1024,
            60,
        )),
        WorkerLimits::default(),
    )
    .open(Arc::new(b"just words".to_vec()));
    assert!(matches!(
        refused,
        Err(sven_document::DocumentError::Unsupported(_))
    ));
}
