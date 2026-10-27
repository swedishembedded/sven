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

use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{render, render_into, PixmapSettings, RenderCache, RenderSettings};

use crate::columns::column_profile;
use crate::error::DocumentError;

/// One rendered page, PNG encoded.
pub struct RenderedPage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// No ink at all: nothing for a model to read.
    pub blank: bool,
    /// Text columns the page looks to have (1 for ordinary text).
    pub columns: usize,
    /// Share of dark pixels over the strips holding text (see
    /// [`crate::columns::column_profile`]).
    pub ink: f32,
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
    /// Render only `bbox` (`[x1, y1, x2, y2]` as fractions of the page) of
    /// page `index` at `dpi`.
    fn render_region(
        &self,
        index: usize,
        dpi: f32,
        bbox: [f32; 4],
    ) -> Result<RenderedPage, DocumentError>;
    /// Several regions of one page. A backend may share work between them
    /// (decoding the page's images once); the default renders one by one.
    fn render_regions(
        &self,
        index: usize,
        dpi: f32,
        boxes: &[[f32; 4]],
    ) -> Vec<Result<RenderedPage, DocumentError>> {
        boxes
            .iter()
            .map(|b| self.render_region(index, dpi, *b))
            .collect()
    }
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
        let (columns, ink) = if blank {
            (1, 0.0)
        } else {
            column_profile(rendered.data_as_u8_slice(), width as usize, height as usize)
        };
        let png = rendered
            .into_png()
            .map_err(|e| DocumentError::InvalidPdf(format!("PNG encoding failed: {e}")))?;
        Ok(RenderedPage {
            png,
            width,
            height,
            blank,
            columns,
            ink,
        })
    }

    fn render_region(
        &self,
        index: usize,
        dpi: f32,
        bbox: [f32; 4],
    ) -> Result<RenderedPage, DocumentError> {
        self.render_regions(index, dpi, &[bbox]).remove(0)
    }

    fn render_regions(
        &self,
        index: usize,
        dpi: f32,
        boxes: &[[f32; 4]],
    ) -> Vec<Result<RenderedPage, DocumentError>> {
        let fail = |e: DocumentError| boxes.iter().map(|_| Err(clone_error(&e))).collect();
        let pdf = match parse(&self.data) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let Some(page) = pdf.pages().get(index) else {
            return fail(DocumentError::InvalidPdf(format!("no page {}", index + 1)));
        };
        // One cache for all regions: the page's images are decoded once.
        let cache = RenderCache::new();
        boxes
            .iter()
            .map(|bbox| render_block(page, &cache, index, dpi, *bbox))
            .collect()
    }
}

fn clone_error(e: &DocumentError) -> DocumentError {
    match e {
        DocumentError::Encrypted => DocumentError::Encrypted,
        other => DocumentError::InvalidPdf(other.to_string()),
    }
}

fn render_block<'a>(
    page: &'a hayro::hayro_syntax::page::Page<'a>,
    cache: &RenderCache<'a>,
    index: usize,
    dpi: f32,
    bbox: [f32; 4],
) -> Result<RenderedPage, DocumentError> {
    let (w, h) = page.render_dimensions();
    let mut scale = dpi / 72.0;
    let longest = ((bbox[2] - bbox[0]) * w).max((bbox[3] - bbox[1]) * h) * scale;
    if longest > MAX_SIDE_PX {
        scale *= MAX_SIDE_PX / longest;
    }
    // Sideways margin keeps glyph edges that the model's box clips. None
    // above and below: neighbouring blocks share an edge, and a margin there
    // would pull a sliver of the next line into the crop.
    let pad = 0.004;
    let x0 = ((bbox[0] - pad).max(0.0) * w * scale).floor();
    let y0 = (bbox[1].max(0.0) * h * scale).floor();
    let x1 = ((bbox[2] + pad).min(1.0) * w * scale).ceil();
    let y1 = (bbox[3].min(1.0) * h * scale).ceil();
    let (cw, ch) = (((x1 - x0) as u16).max(8), ((y1 - y0) as u16).max(8));
    let rendered = catch_unwind(AssertUnwindSafe(|| {
        let mut ctx = RenderContext::new(cw, ch);
        let transform = Affine::translate((-f64::from(x0), -f64::from(y0)))
            * Affine::scale(f64::from(scale))
            * page.initial_transform(true).to_kurbo();
        render_into(
            page,
            cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &mut ctx,
            transform,
        );
        ctx.flush();
        let mut pixmap = Pixmap::new(cw, ch);
        ctx.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..Default::default()
            },
        );
        pixmap
    }))
    .map_err(|_| DocumentError::InvalidPdf(format!("rendering page {} panicked", index + 1)))?;
    let blank = rendered
        .data_as_u8_slice()
        .iter()
        .all(|channel| *channel >= 250);
    let png = rendered
        .into_png()
        .map_err(|e| DocumentError::InvalidPdf(format!("PNG encoding failed: {e}")))?;
    Ok(RenderedPage {
        png,
        width: u32::from(cw),
        height: u32::from(ch),
        blank,
        columns: 1,
        ink: 0.0,
    })
}
