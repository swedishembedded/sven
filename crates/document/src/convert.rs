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

use base64::Engine as _;
use futures::StreamExt;
use serde::Serialize;
use sven_model::{
    CompletionRequest, ContentPart, Message, ModelProvider, ResponseEvent, SamplingParams,
};
use tokio::sync::Semaphore;

use crate::assemble::{assemble, AssembleOptions};
use crate::backend::{PdfBackend, PdfSource, RenderedPage};
use crate::error::DocumentError;
use crate::manifest::{
    manifest_path, page_key, sha256_hex, write_atomic, Manifest, Method, PageRecord, Status,
    MANIFEST_VERSION,
};
use crate::pages::parse_selection;
use crate::quality::{assess, TextQuality};
use crate::validate::{check, clean, Problem};

/// Default prompt: plain Markdown. Deliberately without a grounding marker,
/// which would switch a grounding-capable model into bounding-box output.
pub const DEFAULT_PROMPT: &str = "Convert the document to markdown.";

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
    /// Model requests in flight at once. This is client concurrency, not the
    /// server's batch size.
    pub concurrency: usize,
    /// Pages rendered at once.
    pub render_workers: usize,
    pub dpi: f32,
    pub max_tokens: u32,
    pub prompt: String,
    /// Attempts per page; each retry renders at a higher resolution.
    pub max_attempts: u32,
    pub request_timeout: Duration,
    pub page_markers: bool,
    pub dedupe_headers: bool,
    /// Reuse accepted pages of an earlier run's manifest.
    pub resume: bool,
    /// Names the model in page identities and the manifest.
    pub model_spec: String,
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
            max_attempts: 3,
            request_timeout: Duration::from_secs(900),
            page_markers: false,
            dedupe_headers: false,
            resume: true,
            model_spec: String::new(),
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
    pub elapsed_ms: u64,
}

/// Called after each page settles with `(done, total, record)`.
pub type Progress = Arc<dyn Fn(usize, usize, &PageRecord) + Send + Sync>;

pub struct Converter {
    backend: Arc<dyn PdfBackend>,
    /// Replicas of one model; requests are spread across them.
    providers: Vec<Arc<dyn ModelProvider>>,
    progress: Option<Progress>,
}

impl Converter {
    /// `providers` are interchangeable endpoints of the same model.
    pub fn new(backend: Arc<dyn PdfBackend>, providers: Vec<Arc<dyn ModelProvider>>) -> Self {
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
        let doc: Arc<dyn PdfSource> = {
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
        let plan = plan_pages(&selected, &native, options, &source_sha);
        let needs_ocr = plan.iter().any(|p| p.method == Method::Ocr);
        if needs_ocr && self.providers.is_empty() {
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
        let done = Arc::new(Mutex::new(reused));
        let ctx = Arc::new(PageContext {
            doc,
            providers: self.providers.clone(),
            next: std::sync::atomic::AtomicUsize::new(0),
            options: options.clone(),
            render_slots: Semaphore::new(options.render_workers.max(1)),
        });
        let mut results = futures::stream::iter(todo)
            .map(|work| {
                let ctx = Arc::clone(&ctx);
                let native = native[work.page - 1].clone();
                async move { process_page(&ctx, work, native).await }
            })
            .buffer_unordered(options.concurrency.max(1));
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
        Ok(summarise(destination, &pages, output_sha, reused, started))
    }
}

fn summarise(
    destination: &Path,
    pages: &[PageRecord],
    sha256: String,
    reused: usize,
    started: Instant,
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
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, DocumentError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DocumentError::InvalidPdf(format!("worker failed: {e}")))
}

// ── Planning ────────────────────────────────────────────────────────────────

struct PageWork {
    /// 1-based.
    page: usize,
    method: Method,
    key: String,
    /// Native text when it is good enough to compare an OCR answer against.
    reference: bool,
}

fn plan_pages(
    selected: &[usize],
    native: &[Option<String>],
    options: &ConvertOptions,
    source_sha: &str,
) -> Vec<PageWork> {
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
                Method::Ocr => page_key(&[
                    source_sha,
                    &page.to_string(),
                    "ocr",
                    &options.dpi.to_string(),
                    &options.model_spec,
                    &options.prompt,
                    &options.max_tokens.to_string(),
                ]),
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

// ── One page ────────────────────────────────────────────────────────────────

struct PageContext {
    doc: Arc<dyn PdfSource>,
    providers: Vec<Arc<dyn ModelProvider>>,
    next: std::sync::atomic::AtomicUsize,
    options: ConvertOptions,
    render_slots: Semaphore,
}

async fn process_page(ctx: &PageContext, work: PageWork, native: Option<String>) -> PageRecord {
    let mut record = PageRecord {
        page: work.page,
        key: work.key.clone(),
        method: work.method,
        status: Status::Ok,
        model: None,
        attempts: 0,
        dpi: None,
        sha256: String::new(),
        text: String::new(),
        warnings: Vec::new(),
        render_ms: 0,
        ocr_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
    };
    match work.method {
        Method::Ocr => recognise(ctx, &work, native.as_deref(), &mut record).await,
        _ => {
            record.text = native.as_deref().unwrap_or_default().trim().to_string();
            if record.text.is_empty() {
                record.status = Status::Warning;
                record.warnings = vec!["the page has no native text".into()];
            } else if !work.reference {
                record.status = Status::Warning;
                record.warnings = vec!["the native text layer looks unreliable".into()];
            }
        }
    }
    record.sha256 = sha256_hex(record.text.as_bytes());
    record
}

/// Fills `record` for an OCR page: render, ask, validate, and retry at a
/// higher resolution while the budget lasts.
async fn recognise(
    ctx: &PageContext,
    work: &PageWork,
    native: Option<&str>,
    record: &mut PageRecord,
) {
    let opts = &ctx.options;
    record.model = Some(opts.model_spec.clone());
    let mut best: Option<(Vec<Problem>, String, f32)> = None;
    let mut last_error = None;
    for attempt in 1..=opts.max_attempts.max(1) {
        record.attempts = attempt;
        let dpi = opts.dpi * (1.0 + 0.5 * (attempt - 1) as f32);
        let rendered = match render(ctx, work.page - 1, dpi, &mut record.render_ms).await {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(format!("render failed: {e}"));
                break;
            }
        };
        if rendered.blank {
            record.method = Method::Blank;
            record.dpi = Some(dpi);
            return;
        }
        let started = Instant::now();
        let outcome = ask(ctx, &rendered).await;
        record.ocr_ms += started.elapsed().as_millis() as u64;
        match outcome {
            Ok(answer) => {
                record.input_tokens += answer.input_tokens;
                record.output_tokens += answer.output_tokens;
                let text = clean(&answer.text);
                let problems = check(&text, answer.cutoff, native.filter(|_| work.reference));
                if problems.is_empty() {
                    record.text = text;
                    record.dpi = Some(dpi);
                    return;
                }
                let better = best.as_ref().is_none_or(|(p, t, _)| {
                    (problems.len(), std::cmp::Reverse(text.len()))
                        < (p.len(), std::cmp::Reverse(t.len()))
                });
                if better {
                    best = Some((problems, text, dpi));
                }
                last_error = None;
            }
            Err(e) => {
                last_error = Some(e);
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
            }
        }
    }
    match best {
        Some((problems, text, dpi)) if !text.is_empty() => {
            record.warnings = problems.iter().map(|p| p.describe().to_string()).collect();
            record.text = text;
            record.dpi = Some(dpi);
            record.status = Status::Warning;
        }
        Some((problems, _, _)) => {
            // Only an empty answer came back every time: a blank page is the
            // plausible reading when the page has no native text either.
            record.warnings = problems.iter().map(|p| p.describe().to_string()).collect();
            record.status = Status::Warning;
        }
        None => {
            record.warnings = vec![last_error.unwrap_or_else(|| "no answer".into())];
            record.status = Status::Failed;
        }
    }
}

async fn render(
    ctx: &PageContext,
    index: usize,
    dpi: f32,
    spent_ms: &mut u64,
) -> Result<RenderedPage, DocumentError> {
    let _slot = ctx
        .render_slots
        .acquire()
        .await
        .expect("semaphore is never closed");
    let doc = Arc::clone(&ctx.doc);
    let started = Instant::now();
    let rendered = blocking(move || doc.render(index, dpi)).await?;
    *spent_ms += started.elapsed().as_millis() as u64;
    rendered
}

struct Answer {
    text: String,
    cutoff: bool,
    input_tokens: u64,
    output_tokens: u64,
}

async fn ask(ctx: &PageContext, page: &RenderedPage) -> Result<Answer, String> {
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&page.png)
    );
    let request = CompletionRequest {
        messages: vec![Message::user_with_parts(vec![
            ContentPart::image(url),
            ContentPart::text(ctx.options.prompt.clone()),
        ])],
        stream: true,
        max_output_tokens_override: Some(ctx.options.max_tokens),
        sampling: SamplingParams {
            temperature: Some(0.0),
            ..Default::default()
        },
        ..Default::default()
    };
    let exchange = async {
        let provider = &ctx.providers
            [ctx.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % ctx.providers.len()];
        let mut stream = provider
            .complete(request)
            .await
            .map_err(|e| format!("model request failed: {e}"))?;
        let mut answer = Answer {
            text: String::new(),
            cutoff: false,
            input_tokens: 0,
            output_tokens: 0,
        };
        while let Some(event) = stream.next().await {
            match event.map_err(|e| format!("model stream failed: {e}"))? {
                ResponseEvent::TextDelta(t) => answer.text.push_str(&t),
                ResponseEvent::MaxTokens => answer.cutoff = true,
                ResponseEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    answer.input_tokens += u64::from(input_tokens);
                    answer.output_tokens += u64::from(output_tokens);
                }
                ResponseEvent::Error(e) => return Err(format!("model error: {e}")),
                _ => {}
            }
        }
        Ok(answer)
    };
    tokio::time::timeout(ctx.options.request_timeout, exchange)
        .await
        .map_err(|_| "model request timed out".to_string())?
}

// ── Manifest checkpointing ──────────────────────────────────────────────────

/// Longest time a finished page may exist only in memory.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(2);

struct Checkpoint {
    destination: PathBuf,
    manifest: Manifest,
    last: Instant,
}

impl Checkpoint {
    fn new(destination: &Path, manifest: Manifest) -> Self {
        Self {
            destination: destination.to_path_buf(),
            manifest,
            last: Instant::now(),
        }
    }

    fn push(&mut self, record: PageRecord, checkpoint: bool) {
        self.manifest.pages.retain(|p| p.page != record.page);
        self.manifest.pages.push(record);
        if checkpoint && self.last.elapsed() >= CHECKPOINT_EVERY {
            if let Err(e) = self.flush() {
                tracing::warn!(error = %e, "manifest checkpoint failed");
            }
        }
    }

    fn flush(&mut self) -> Result<(), DocumentError> {
        self.last = Instant::now();
        self.manifest.pages.sort_by_key(|p| p.page);
        self.manifest.write(&self.destination)
    }
}

struct FlushOnDrop(Arc<Mutex<Checkpoint>>);

impl Drop for FlushOnDrop {
    fn drop(&mut self) {
        if let Ok(mut cp) = self.0.lock() {
            if !cp.manifest.pages.is_empty() {
                let _ = cp.flush();
            }
        }
    }
}
