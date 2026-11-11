// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The conversion pipeline: inspect, classify, render, recognise, validate,
//! assemble, publish.
//!
//! Pages are independent units of work. At most `concurrency` pages are in
//! flight, and a page image exists only while its page is in flight, so memory
//! is bounded by `concurrency` images however long the document is. Dropping
//! the future returned by [`Converter::convert`] cancels every request; the
//! manifest is flushed on the way out, so a rerun resumes from it.
//!
//! Swedish Embedded AB implements document digitisation pipelines for its
//! clients. If your team needs expertise in converting large scanned archives
//! into verified, searchable text then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::Serialize;
use sven_model::ModelProvider;
use tokio::sync::Semaphore;

use crate::assemble::{assemble, AssembleOptions};
use crate::backend::{DocumentBackend, DocumentSource};
use crate::batch::{fitted_size, process_chunk, Batching};
use crate::checkpoint::{Checkpoint, FlushOnDrop};
use crate::error::DocumentError;
use crate::layout::DeepseekGrounding;
use crate::limiter::{Limiter, LimiterStats};
use crate::manifest::{
    manifest_path, page_key, sha256_hex, write_atomic, Manifest, Method, PageRecord, Status,
    MANIFEST_VERSION,
};
use crate::page::{render, PageContext, PageWork, Prepared};
use crate::pages::parse_selection;
use crate::quality::{assess, TextQuality};

/// Default prompt: plain Markdown. Deliberately without a grounding marker,
/// which would switch a grounding-capable model into bounding-box output.
pub const DEFAULT_PROMPT: &str = "Convert the document to markdown.";

/// Whether a page is read block by block from its layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutMode {
    /// Pages that look like columns are, and so is a page whose whole-page
    /// reading was cut off or looped.
    #[default]
    Auto,
    /// Never: every page is read whole.
    Off,
    /// Every OCR page.
    Regions,
}

impl std::str::FromStr for LayoutMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "off" => Ok(Self::Off),
            "regions" => Ok(Self::Regions),
            other => Err(format!("'{other}' is not one of auto, off, regions")),
        }
    }
}

/// Instruction that makes a grounding-capable model return each block with
/// its location. A model without grounding answers with plain Markdown, which
/// is then taken as the reading of the page.
pub const DEFAULT_LAYOUT_PROMPT: &str = "<|grounding|>Convert the document to markdown.";

/// Which pages go to the OCR model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OcrMode {
    /// Pages whose native text is good keep it; the rest are recognised.
    #[default]
    Auto,
    /// Every page is recognised.
    Always,
    /// Native text only; no model is called.
    Never,
}

impl std::str::FromStr for OcrMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(format!("'{other}' is not one of auto, always, never")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub ocr: OcrMode,
    /// 1-based selection such as `1-3,7`; empty selects every page.
    pub pages: String,
    /// Most model requests in flight at once per endpoint. The window starts
    /// here and shrinks while the server pushes back (429/503, timeouts). This
    /// is client concurrency, not the server's batch size.
    pub concurrency: usize,
    /// Pages rendered at once.
    pub render_workers: usize,
    pub dpi: f32,
    pub max_tokens: u32,
    pub prompt: String,
    pub layout: LayoutMode,
    /// Instruction for the layout pass.
    pub layout_prompt: String,
    /// Resolution blocks are read at in the layout pass.
    pub region_dpi: f32,
    /// Attempts per page; each retry renders at a higher resolution.
    pub max_attempts: u32,
    pub request_timeout: Duration,
    pub page_markers: bool,
    pub dedupe_headers: bool,
    /// Reuse accepted pages of an earlier run's manifest.
    pub resume: bool,
    /// Names the model in page identities and the manifest.
    pub model_spec: String,
    /// Pages read in one request (at most
    /// [`crate::MAX_PAGES_PER_REQUEST`], and no more than the model's listed
    /// output limit holds `max_tokens` allowances); `1`, the default, reads
    /// every page alone.
    pub pages_per_request: usize,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            ocr: OcrMode::Auto,
            pages: String::new(),
            concurrency: 16,
            render_workers: std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
            dpi: 150.0,
            max_tokens: 4096,
            prompt: DEFAULT_PROMPT.to_string(),
            layout: LayoutMode::Auto,
            layout_prompt: DEFAULT_LAYOUT_PROMPT.to_string(),
            region_dpi: 300.0,
            max_attempts: 3,
            request_timeout: Duration::from_secs(900),
            page_markers: false,
            dedupe_headers: false,
            resume: true,
            model_spec: String::new(),
            pages_per_request: 1,
        }
    }
}

/// What a finished run reports. Never contains page text.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub output: PathBuf,
    pub manifest: PathBuf,
    pub sha256: String,
    pub pages_total: usize,
    pub pages_native: usize,
    pub pages_ocr: usize,
    pub pages_blank: usize,
    /// Pages taken from an earlier run's manifest rather than redone.
    pub pages_reused: usize,
    /// Pages kept with a failed check.
    pub pages_warning: usize,
    pub pages_failed: usize,
    pub warnings: Vec<String>,
    /// Why pages failed: one entry per distinct cause with the pages it hit.
    pub failures: Vec<String>,
    pub elapsed_ms: u64,
    /// Most model requests in flight at once.
    pub max_in_flight: usize,
    /// Time with no request in flight although OCR pages remained.
    pub backend_idle_ms: u64,
    /// Requests answered by each endpoint.
    pub requests_per_endpoint: Vec<u64>,
    /// Pages accepted from a request that carried several.
    pub pages_batched: usize,
    /// What the run as a whole has to say (not about one page).
    pub notes: Vec<String>,
}

/// How a conversion would read its pages ([`Converter::count`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PageCounts {
    pub total: usize,
    /// Pages whose own text layer is kept: no model request.
    pub native: usize,
    /// Pages that would go to the OCR model.
    pub ocr: usize,
}

/// Called after each page settles with `(done, total, record)`.
pub type Progress = Arc<dyn Fn(usize, usize, &PageRecord) + Send + Sync>;

pub struct Converter {
    backend: Arc<dyn DocumentBackend>,
    /// Replicas of one model; requests are spread across them.
    providers: Vec<Arc<dyn ModelProvider>>,
    progress: Option<Progress>,
}

impl Converter {
    /// `providers` are interchangeable endpoints of the same model.
    pub fn new(backend: Arc<dyn DocumentBackend>, providers: Vec<Arc<dyn ModelProvider>>) -> Self {
        Self {
            backend,
            providers,
            progress: None,
        }
    }

    pub fn with_progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// How the selected pages of `source` would be read, without asking the
    /// model anything: how many keep their text layer and how many would go
    /// to the OCR model (blank ones among them cost nothing, which shows only
    /// once they are rendered).
    pub async fn count(
        &self,
        source: &Path,
        options: &ConvertOptions,
    ) -> Result<PageCounts, DocumentError> {
        let data = Arc::new(
            tokio::fs::read(source)
                .await
                .map_err(|e| DocumentError::Read {
                    path: source.display().to_string(),
                    source: e,
                })?,
        );
        let doc: Arc<dyn DocumentSource> = {
            let (backend, data) = (Arc::clone(&self.backend), Arc::clone(&data));
            blocking(move || backend.open(data)).await??
        };
        let selected = parse_selection(&options.pages, doc.page_count())?;
        let native: Vec<Option<String>> = if options.ocr == OcrMode::Always {
            vec![None; doc.page_count()]
        } else {
            let doc = Arc::clone(&doc);
            blocking(move || doc.native_text()).await?
        };
        let plan = plan_pages(&selected, &native, options, &sha256_hex(&data), 1);
        let ocr = plan.iter().filter(|p| p.method == Method::Ocr).count();
        Ok(PageCounts {
            total: plan.len(),
            native: plan.len() - ocr,
            ocr,
        })
    }

    pub async fn convert(
        &self,
        source: &Path,
        destination: &Path,
        options: &ConvertOptions,
    ) -> Result<Report, DocumentError> {
        let started = Instant::now();
        let data = Arc::new(
            tokio::fs::read(source)
                .await
                .map_err(|e| DocumentError::Read {
                    path: source.display().to_string(),
                    source: e,
                })?,
        );
        let source_sha = sha256_hex(&data);
        let doc: Arc<dyn DocumentSource> = {
            let (backend, data) = (Arc::clone(&self.backend), Arc::clone(&data));
            blocking(move || backend.open(data)).await??
        };
        let selected = parse_selection(&options.pages, doc.page_count())?;
        let native: Vec<Option<String>> = if options.ocr == OcrMode::Always {
            vec![None; doc.page_count()]
        } else {
            let doc = Arc::clone(&doc);
            blocking(move || doc.native_text()).await?
        };

        let previous = options
            .resume
            .then(|| Manifest::load_matching(destination, &source_sha))
            .flatten();
        let batching_size = fitted_size(
            options.pages_per_request,
            &self.providers,
            options.max_tokens,
        );
        let plan = plan_pages(&selected, &native, options, &source_sha, batching_size);
        let needs_ocr = plan.iter().any(|p| p.method == Method::Ocr);
        // Without a model the pages that have a good text layer still
        // convert and the rest fail with that cause; refused only when no
        // page could convert, or when OCR was asked for outright.
        let only_ocr = plan.iter().all(|p| p.method == Method::Ocr);
        if needs_ocr && self.providers.is_empty() && (only_ocr || options.ocr == OcrMode::Always) {
            return Err(DocumentError::NoModel);
        }
        if needs_ocr && self.providers.iter().any(|p| !p.supports_images()) {
            return Err(DocumentError::Model(format!(
                "model '{}' does not declare image input. Pick an image-capable model, or declare \
                 it on the provider entry with `input_modalities: [text, image]`.",
                options.model_spec
            )));
        }

        let state = Arc::new(Mutex::new(Checkpoint::new(
            destination,
            Manifest {
                version: MANIFEST_VERSION,
                source_sha256: source_sha.clone(),
                page_count: doc.page_count(),
                pages: Vec::new(),
                output_sha256: None,
            },
        )));
        // Flushes the manifest when this future is dropped before it ends.
        let _flush = FlushOnDrop(Arc::clone(&state));

        let total = plan.len();
        let mut reused = 0;
        let mut todo = Vec::new();
        for work in plan {
            match previous
                .as_ref()
                .and_then(|m| m.reusable(work.page, &work.key))
            {
                Some(record) => {
                    reused += 1;
                    state.lock().unwrap().push(record.clone(), false);
                }
                None => todo.push(work),
            }
        }
        let workers = options.concurrency.max(1) * self.providers.len().max(1);
        let batch = batching_size;
        let ocr_pages = todo.iter().filter(|w| w.method == Method::Ocr).count();
        let limiter = Limiter::new(self.providers.len(), options.concurrency, ocr_pages);
        let ctx = Arc::new(PageContext {
            doc,
            providers: self.providers.clone(),
            limiter: Arc::clone(&limiter),
            options: options.clone(),
            render_slots: Semaphore::new(options.render_workers.max(1)),
            parser: Arc::new(DeepseekGrounding),
            batching: Batching::new(batching_size),
        });
        let done = Arc::new(Mutex::new(reused));
        // Stage one renders up to `workers` pages ahead of the model; stage
        // two keeps `workers` pages at the model. A finished render waits
        // only while stage two is full, so at most 2 x workers page images
        // exist at any time, however long the document is.
        let mut results = futures::stream::iter(todo)
            .map(|work| {
                let ctx = Arc::clone(&ctx);
                let native = native[work.page - 1].clone();
                async move {
                    let prepared = if work.method == Method::Ocr && !ctx.providers.is_empty() {
                        let (image, render_ms) = render(&ctx, work.page - 1, ctx.options.dpi).await;
                        Some(Prepared { image, render_ms })
                    } else {
                        None
                    };
                    (work, native, prepared)
                }
            })
            .buffer_unordered(workers)
            // Pages go to the model a batch at a time; a batch is one
            // request when the model reads pages together, else its pages
            // go side by side. Either way `workers` pages are at the model.
            .chunks(batch)
            .map(|chunk| {
                let ctx = Arc::clone(&ctx);
                async move { process_chunk(&ctx, chunk).await }
            })
            .buffer_unordered((workers / batch).max(1))
            .flat_map(futures::stream::iter);
        while let Some(record) = results.next().await {
            let n = {
                let mut d = done.lock().unwrap();
                *d += 1;
                *d
            };
            if let Some(progress) = &self.progress {
                progress(n, total, &record);
            }
            state.lock().unwrap().push(record, true);
        }
        drop(results);
        let stats = limiter.stats();

        let mut pages = state.lock().unwrap().manifest.pages.clone();
        pages.sort_by_key(|p| p.page);
        let markdown = assemble(
            &pages,
            AssembleOptions {
                page_markers: options.page_markers,
                dedupe_headers: options.dedupe_headers,
            },
        );
        let output_sha = sha256_hex(markdown.as_bytes());
        {
            let mut cp = state.lock().unwrap();
            cp.manifest.output_sha256 = Some(output_sha.clone());
            cp.flush()?;
        }
        write_atomic(destination, markdown.as_bytes())?;
        let mut report = summarise(destination, &pages, output_sha, reused, started, stats);
        report.pages_batched = ctx.batching.pages_batched();
        report.notes = ctx.batching.notes();
        Ok(report)
    }
}

fn summarise(
    destination: &Path,
    pages: &[PageRecord],
    sha256: String,
    reused: usize,
    started: Instant,
    stats: LimiterStats,
) -> Report {
    let count = |m: Method| pages.iter().filter(|p| p.method == m).count();
    let by_status = |s: Status| pages.iter().filter(|p| p.status == s).count();
    let warnings = pages
        .iter()
        .flat_map(|p| {
            p.warnings
                .iter()
                .map(move |w| format!("page {}: {w}", p.page))
        })
        .collect();
    Report {
        output: destination.to_path_buf(),
        manifest: manifest_path(destination),
        sha256,
        pages_total: pages.len(),
        pages_native: count(Method::Native),
        pages_ocr: count(Method::Ocr),
        pages_blank: count(Method::Blank),
        pages_reused: reused,
        pages_warning: by_status(Status::Warning),
        pages_failed: by_status(Status::Failed),
        warnings,
        failures: failure_causes(pages),
        elapsed_ms: started.elapsed().as_millis() as u64,
        max_in_flight: stats.peak_in_flight,
        backend_idle_ms: stats.idle_ms,
        requests_per_endpoint: stats.requests_per_endpoint,
        pages_batched: 0,
        notes: Vec::new(),
    }
}

/// The distinct causes of failed pages, each with the pages it hit.
fn failure_causes(pages: &[PageRecord]) -> Vec<String> {
    let mut causes: Vec<(String, Vec<usize>)> = Vec::new();
    for page in pages.iter().filter(|p| p.status == Status::Failed) {
        let cause = page
            .warnings
            .first()
            .cloned()
            .unwrap_or_else(|| "no answer".into());
        match causes.iter_mut().find(|(c, _)| *c == cause) {
            Some((_, hit)) => hit.push(page.page),
            None => causes.push((cause, vec![page.page])),
        }
    }
    causes
        .into_iter()
        .map(|(cause, hit)| {
            let list: Vec<String> = hit.iter().take(8).map(usize::to_string).collect();
            let more = if hit.len() > 8 { ", ..." } else { "" };
            format!("pages {}{more}: {cause}", list.join(", "))
        })
        .collect()
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, DocumentError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DocumentError::Invalid(format!("worker failed: {e}")))
}

// ── Planning ────────────────────────────────────────────────────────────────

fn plan_pages(
    selected: &[usize],
    native: &[Option<String>],
    options: &ConvertOptions,
    source_sha: &str,
    pages_per_request: usize,
) -> Vec<PageWork> {
    // Reading pages together changes the request, so it is part of a page's
    // identity; a page read alone keeps the identity earlier runs gave it.
    let together = (pages_per_request > 1).then(|| format!("batch {pages_per_request}"));
    selected
        .iter()
        .map(|&index| {
            let page = index + 1;
            let quality = native[index].as_deref().map(assess);
            let method = match options.ocr {
                OcrMode::Never => Method::Native,
                OcrMode::Always => Method::Ocr,
                OcrMode::Auto if quality == Some(TextQuality::Good) => Method::Native,
                OcrMode::Auto => Method::Ocr,
            };
            let key = match method {
                Method::Ocr => {
                    let page = page.to_string();
                    let dpi = options.dpi.to_string();
                    let max_tokens = options.max_tokens.to_string();
                    let layout = format!("{:?}", options.layout);
                    let region_dpi = options.region_dpi.to_string();
                    let mut parts = vec![
                        source_sha,
                        page.as_str(),
                        "ocr",
                        dpi.as_str(),
                        options.model_spec.as_str(),
                        options.prompt.as_str(),
                        max_tokens.as_str(),
                        layout.as_str(),
                        options.layout_prompt.as_str(),
                        region_dpi.as_str(),
                    ];
                    parts.extend(together.as_deref());
                    page_key(&parts)
                }
                _ => page_key(&[source_sha, &page.to_string(), "native"]),
            };
            PageWork {
                page,
                method,
                key,
                reference: quality == Some(TextQuality::Good),
            }
        })
        .collect()
}
