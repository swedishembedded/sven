// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A document read by workers of its own (`IsolatedBackend`): the same
//! conversion as in process, and a page that hangs or kills its worker costs
//! that page alone. The workers here are threads behind pipes, speaking the
//! same protocol a worker process speaks.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::common::{build_pdf, FixturePage, Reply, ScriptedOcr, PROSE};
use sven_document::testing::build_pdf_with_outline;
use sven_document::{
    serve_worker, ConvertOptions, Converter, DocumentBackend, DocumentError, DocumentSource,
    HayroBackend, IsolatedBackend, OutlineEntry, RenderedPage, WorkerLimits, WorkerProcess,
    WorkerSpawner,
};

/// Renders as hayro does, except that page `hang` never finishes and page
/// `crash` takes its worker down.
struct Trouble {
    hang: Option<usize>,
    crash: Option<usize>,
    /// Every page takes this long.
    slow: Duration,
}

struct TroubleSource {
    inner: Arc<dyn DocumentSource>,
    hang: Option<usize>,
    crash: Option<usize>,
    slow: Duration,
}

impl DocumentBackend for Trouble {
    fn open(&self, data: Arc<Vec<u8>>) -> Result<Arc<dyn DocumentSource>, DocumentError> {
        Ok(Arc::new(TroubleSource {
            inner: HayroBackend.open(data)?,
            hang: self.hang,
            crash: self.crash,
            slow: self.slow,
        }))
    }
}

impl DocumentSource for TroubleSource {
    fn page_count(&self) -> usize {
        self.inner.page_count()
    }
    fn native_text(&self) -> Vec<Option<String>> {
        self.inner.native_text()
    }
    fn outline(&self) -> Result<Vec<OutlineEntry>, DocumentError> {
        self.inner.outline()
    }
    fn render(&self, index: usize, dpi: f32) -> Result<RenderedPage, DocumentError> {
        if Some(index) == self.hang {
            std::thread::sleep(Duration::from_secs(30));
        }
        assert!(Some(index) != self.crash, "a renderer bug");
        std::thread::sleep(self.slow);
        self.inner.render(index, dpi)
    }
    fn render_region(
        &self,
        index: usize,
        dpi: f32,
        bbox: [f32; 4],
    ) -> Result<RenderedPage, DocumentError> {
        self.inner.render_region(index, dpi, bbox)
    }
}

/// Workers that are threads serving the protocol over pipes.
struct Threads {
    backend: Arc<dyn DocumentBackend>,
    started: Arc<AtomicUsize>,
}

impl WorkerSpawner for Threads {
    fn spawn(&self) -> std::io::Result<WorkerProcess> {
        let (mut request_in, request_out) = std::io::pipe()?;
        let (answer_out, mut answer_in) = std::io::pipe()?;
        let backend = Arc::clone(&self.backend);
        self.started.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            let _ = serve_worker(&mut request_in, &mut answer_in, &*backend);
        });
        Ok(WorkerProcess {
            input: Box::new(request_out),
            output: Box::new(answer_out),
            // A thread cannot be killed: dropping its pipes ends its loop.
            stop: Box::new(|| {}),
        })
    }
}

fn isolated(
    backend: Arc<dyn DocumentBackend>,
    render: Duration,
) -> (IsolatedBackend, Arc<AtomicUsize>) {
    isolated_within(backend, render, WorkerLimits::default().total)
}

fn isolated_within(
    backend: Arc<dyn DocumentBackend>,
    render: Duration,
    total: Duration,
) -> (IsolatedBackend, Arc<AtomicUsize>) {
    let started = Arc::new(AtomicUsize::new(0));
    let limits = WorkerLimits {
        workers: 2,
        render,
        total,
        ..WorkerLimits::default()
    };
    let spawner = Threads {
        backend,
        started: Arc::clone(&started),
    };
    (IsolatedBackend::new(Arc::new(spawner), limits), started)
}

fn options() -> ConvertOptions {
    ConvertOptions {
        model_spec: "scripted/ocr".into(),
        dpi: 40.0,
        max_attempts: 1,
        ..Default::default()
    }
}

fn pages() -> Vec<FixturePage> {
    vec![
        FixturePage::Text(PROSE.to_vec()),
        FixturePage::Scan(100),
        FixturePage::Scan(120),
        FixturePage::Scan(140),
    ]
}

async fn convert(backend: Arc<dyn DocumentBackend>) -> (sven_document::Report, String) {
    convert_pages(backend, &pages()).await
}

async fn convert_pages(
    backend: Arc<dyn DocumentBackend>,
    pages: &[FixturePage],
) -> (sven_document::Report, String) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("in.pdf");
    std::fs::write(&src, build_pdf(pages)).unwrap();
    let out = dir.path().join("out.md");
    let model = ScriptedOcr::new(|seen, _| Reply::text(format!("scan {}", seen.gray)));
    let report = Converter::new(backend, vec![Arc::new(model)])
        .convert(&src, &out, &options())
        .await
        .unwrap();
    (report, std::fs::read_to_string(&out).unwrap())
}

#[tokio::test]
async fn a_document_read_by_workers_converts_as_it_does_in_process() {
    let (backend, started) = isolated(Arc::new(HayroBackend), Duration::from_secs(30));
    let (isolated_report, isolated_md) = convert(Arc::new(backend)).await;
    let (local_report, local_md) = convert(Arc::new(HayroBackend)).await;
    assert_eq!(isolated_md, local_md);
    assert_eq!(
        (
            isolated_report.pages_native,
            isolated_report.pages_ocr,
            isolated_report.pages_failed
        ),
        (
            local_report.pages_native,
            local_report.pages_ocr,
            local_report.pages_failed
        )
    );
    assert!(
        started.load(Ordering::SeqCst) <= 2,
        "never more workers than allowed"
    );

    // The outline comes from a worker too.
    let pdf = build_pdf_with_outline(&pages(), &[(1, "Scans", 1)]);
    let (backend, _) = isolated(Arc::new(HayroBackend), Duration::from_secs(30));
    let source = backend.open(Arc::new(pdf)).unwrap();
    let outline = source.outline().unwrap();
    assert_eq!(outline.len(), 1);
    assert_eq!(
        (outline[0].title.as_str(), outline[0].first_page),
        ("Scans", 2)
    );
}

#[tokio::test]
async fn a_page_that_hangs_its_worker_is_stopped_at_its_deadline_and_the_rest_converts() {
    let trouble = Trouble {
        hang: Some(2),
        crash: None,
        slow: Duration::ZERO,
    };
    let (backend, started) = isolated(Arc::new(trouble), Duration::from_secs(1));
    let began = Instant::now();
    let (report, md) = convert(Arc::new(backend)).await;
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "it did not wait for the hung page"
    );
    assert_eq!(report.pages_failed, 1);
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.contains("took longer than")),
        "{:?}",
        report.failures
    );
    assert!(md.contains("scan 100") && md.contains("scan 140"), "{md}");
    assert!(
        started.load(Ordering::SeqCst) >= 2,
        "the hung worker was replaced"
    );
}

#[tokio::test]
async fn a_page_that_kills_its_worker_fails_alone() {
    let trouble = Trouble {
        hang: None,
        crash: Some(1),
        slow: Duration::ZERO,
    };
    let (backend, _) = isolated(Arc::new(trouble), Duration::from_secs(30));
    let (report, md) = convert(Arc::new(backend)).await;
    assert_eq!((report.pages_failed, report.pages_ocr), (1, 3));
    assert!(md.contains("scan 120") && md.contains("scan 140"), "{md}");
}

#[tokio::test]
async fn a_document_slow_on_every_page_ends_when_its_reading_budget_is_spent() {
    let slow = Trouble {
        hang: None,
        crash: None,
        slow: Duration::from_millis(700),
    };
    let (backend, _) = isolated_within(
        Arc::new(slow),
        Duration::from_secs(30),
        Duration::from_secs(1),
    );
    let scans: Vec<FixturePage> = (0..6).map(|n| FixturePage::Scan(100 + n * 10)).collect();
    let (report, _) = convert_pages(Arc::new(backend), &scans).await;
    assert!(report.pages_failed >= 1, "{report:?}");
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.contains("took over 1 s to read")),
        "{:?}",
        report.failures
    );
}
