// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The PDF side of the converter behind two small traits, so another renderer
//! or text extractor can replace the pure-Rust one without touching the
//! pipeline.
//!
//! Swedish Embedded AB implements document ingestion pipelines for its
//! clients. If your team needs expertise in turning scanned archives into
//! searchable text then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, PixmapSettings, RenderCache, RenderSettings};

use crate::error::DocumentError;

/// One rendered page, PNG encoded.
pub struct RenderedPage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// No ink at all: nothing for a model to read.
    pub blank: bool,
}

/// Opens PDFs.
pub trait PdfBackend: Send + Sync {
    /// Parse `data`. Fails with [`DocumentError::Encrypted`] for a document
    /// that needs a password.
    fn open(&self, data: Arc<Vec<u8>>) -> Result<Arc<dyn PdfSource>, DocumentError>;
}

/// An opened document. Methods block; callers run them on a blocking thread.
pub trait PdfSource: Send + Sync {
    fn page_count(&self) -> usize;
    /// The native text layer of every page, `None` for a page (or a whole
    /// document) whose text could not be extracted.
    fn native_text(&self) -> Vec<Option<String>>;
    /// Render page `index` (0-based) at `dpi`.
    fn render(&self, index: usize, dpi: f32) -> Result<RenderedPage, DocumentError>;
}

/// Longest rendered side in pixels; larger requests are scaled down. Keeps a
/// poster-sized page from producing an image no model accepts.
const MAX_SIDE_PX: f32 = 6000.0;

/// hayro for rendering, pdf-extract for text.
#[derive(Debug, Default, Clone, Copy)]
pub struct HayroBackend;

impl PdfBackend for HayroBackend {
    fn open(&self, data: Arc<Vec<u8>>) -> Result<Arc<dyn PdfSource>, DocumentError> {
        let pages = parse(&data)?.pages().len();
        if pages == 0 {
            return Err(DocumentError::Empty);
        }
        Ok(Arc::new(HayroSource { data, pages }))
    }
}

fn parse(data: &Arc<Vec<u8>>) -> Result<Pdf, DocumentError> {
    Pdf::new(Arc::clone(data)).map_err(|e| match e {
        LoadPdfError::Decryption(_) => DocumentError::Encrypted,
        LoadPdfError::Invalid => DocumentError::InvalidPdf("the file does not parse".into()),
    })
}

struct HayroSource {
    data: Arc<Vec<u8>>,
    pages: usize,
}

impl PdfSource for HayroSource {
    fn page_count(&self) -> usize {
        self.pages
    }

    fn native_text(&self) -> Vec<Option<String>> {
        // The extractor panics on some malformed fonts; a panic must cost the
        // text layer, not the run.
        let extracted = catch_unwind(AssertUnwindSafe(|| {
            pdf_extract::extract_text_from_mem_by_pages(&self.data)
        }));
        match extracted {
            Ok(Ok(pages)) if pages.len() == self.pages => pages.into_iter().map(Some).collect(),
            Ok(Ok(pages)) => {
                tracing::warn!(
                    extracted = pages.len(),
                    renderable = self.pages,
                    "text extractor and renderer disagree on the page count; ignoring the text layer"
                );
                vec![None; self.pages]
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "native text extraction failed");
                vec![None; self.pages]
            }
            Err(_) => {
                tracing::warn!("native text extraction panicked");
                vec![None; self.pages]
            }
        }
    }

    fn render(&self, index: usize, dpi: f32) -> Result<RenderedPage, DocumentError> {
        let pdf = parse(&self.data)?;
        let page = pdf
            .pages()
            .get(index)
            .ok_or_else(|| DocumentError::InvalidPdf(format!("no page {}", index + 1)))?;
        let (w, h) = page.render_dimensions();
        let mut scale = dpi / 72.0;
        let longest = w.max(h) * scale;
        if longest > MAX_SIDE_PX {
            scale *= MAX_SIDE_PX / longest;
        }
        let rendered = catch_unwind(AssertUnwindSafe(|| {
            render(
                page,
                &RenderCache::new(),
                &InterpreterSettings::default(),
                &RenderSettings::default(),
                &PixmapSettings {
                    x_scale: scale,
                    y_scale: scale,
                    bg_color: WHITE,
                },
            )
        }))
        .map_err(|_| DocumentError::InvalidPdf(format!("rendering page {} panicked", index + 1)))?;
        let (width, height) = (u32::from(rendered.width()), u32::from(rendered.height()));
        let blank = rendered
            .data_as_u8_slice()
            .iter()
            .all(|channel| *channel >= 250);
        let png = rendered
            .into_png()
            .map_err(|e| DocumentError::InvalidPdf(format!("PNG encoding failed: {e}")))?;
        Ok(RenderedPage {
            png,
            width,
            height,
            blank,
        })
    }
}
