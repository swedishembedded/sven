// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A picture of a page (a phone photo of a receipt, a scanner's PNG) as a
//! document of one page with no text layer.
//!
//! The picture is decoded once, under bounds that keep a hostile file from
//! taking the machine's memory, and every rendering is a scaled copy of it.
//! A picture has no resolution of its own the pipeline could use, so it is
//! taken to be at [`PICTURE_DPI`]: rendered at that resolution it is its own
//! size, and a retry at a higher one enlarges it as a page would be.
//!
//! Swedish Embedded AB implements document intake pipelines for photographed
//! and scanned paperwork for its clients. If your team needs expertise in
//! turning pictures of documents into structured data then you can procure
//! our services by sending an email to info@swedishembedded.com.

use std::io::Cursor;
use std::sync::Arc;

use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits, RgbaImage};

use crate::backend::{DocumentBackend, DocumentSource, RenderedPage, MAX_SIDE_PX};
use crate::columns::column_profile;
use crate::error::DocumentError;
use crate::locate::OutlineEntry;

/// The resolution a picture is taken to have (the converter's default page
/// resolution, so a picture is sent at its own size by default).
pub const PICTURE_DPI: f32 = 150.0;

/// The formats read, as a person would name them.
pub const PICTURE_FORMATS: &str = "PNG, JPEG, WebP, GIF or BMP";

/// Largest width or height decoded; a page photographed at 50 megapixels fits.
const MAX_DECODED_SIDE: u32 = 12_000;
/// Most memory one decode may allocate.
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

/// Opens pictures; anything else is [`DocumentError::Unsupported`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PictureBackend;

impl DocumentBackend for PictureBackend {
    fn open(&self, data: Arc<Vec<u8>>) -> Result<Arc<dyn DocumentSource>, DocumentError> {
        let unsupported = || {
            DocumentError::Unsupported(format!(
                "the file is neither a PDF nor a picture of a page ({PICTURE_FORMATS})"
            ))
        };
        let format = image::guess_format(&data).map_err(|_| unsupported())?;
        if !matches!(
            format,
            ImageFormat::Png
                | ImageFormat::Jpeg
                | ImageFormat::WebP
                | ImageFormat::Gif
                | ImageFormat::Bmp
        ) {
            return Err(unsupported());
        }
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_DECODED_SIDE);
        limits.max_image_height = Some(MAX_DECODED_SIDE);
        limits.max_alloc = Some(MAX_DECODE_BYTES);
        let mut reader = ImageReader::with_format(Cursor::new(data.as_slice()), format);
        reader.limits(limits);
        let picture = reader
            .decode()
            .map_err(|e| DocumentError::Invalid(format!("the picture does not decode: {e}")))?
            .to_rgba8();
        Ok(Arc::new(PictureSource { picture }))
    }
}

struct PictureSource {
    picture: RgbaImage,
}

impl PictureSource {
    /// `bbox` (fractions of the picture) of the picture at `dpi`.
    fn draw(
        &self,
        dpi: f32,
        bbox: [f32; 4],
        with_columns: bool,
    ) -> Result<RenderedPage, DocumentError> {
        let (w, h) = self.picture.dimensions();
        let x0 = (bbox[0].clamp(0.0, 1.0) * w as f32).floor() as u32;
        let y0 = (bbox[1].clamp(0.0, 1.0) * h as f32).floor() as u32;
        let x1 = ((bbox[2].clamp(0.0, 1.0) * w as f32).ceil() as u32)
            .max(x0 + 1)
            .min(w);
        let y1 = ((bbox[3].clamp(0.0, 1.0) * h as f32).ceil() as u32)
            .max(y0 + 1)
            .min(h);
        let crop = image::imageops::crop_imm(&self.picture, x0, y0, x1 - x0, y1 - y0).to_image();
        let mut scale = dpi / PICTURE_DPI;
        let longest = crop.width().max(crop.height()) as f32 * scale;
        if longest > MAX_SIDE_PX {
            scale *= MAX_SIDE_PX / longest;
        }
        let width = ((crop.width() as f32 * scale).round() as u32).max(8);
        let height = ((crop.height() as f32 * scale).round() as u32).max(8);
        let page = if (width, height) == crop.dimensions() {
            crop
        } else {
            image::imageops::resize(&crop, width, height, FilterType::Triangle)
        };
        let raw = page.as_raw();
        let blank = raw
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px[..3].iter().all(|channel| *channel >= 250));
        let (columns, ink) = if blank || !with_columns {
            (1, 0.0)
        } else {
            column_profile(raw, width as usize, height as usize)
        };
        let mut png = Cursor::new(Vec::new());
        page.write_to(&mut png, ImageFormat::Png)
            .map_err(|e| DocumentError::Invalid(format!("PNG encoding failed: {e}")))?;
        Ok(RenderedPage {
            png: png.into_inner(),
            width,
            height,
            blank,
            columns,
            ink,
        })
    }
}

fn only_page(index: usize) -> Result<(), DocumentError> {
    if index == 0 {
        Ok(())
    } else {
        Err(DocumentError::Invalid(format!(
            "a picture has one page, not a page {}",
            index + 1
        )))
    }
}

impl DocumentSource for PictureSource {
    fn page_count(&self) -> usize {
        1
    }

    fn native_text(&self) -> Vec<Option<String>> {
        vec![None]
    }

    fn outline(&self) -> Result<Vec<OutlineEntry>, DocumentError> {
        Ok(Vec::new())
    }

    fn render(&self, index: usize, dpi: f32) -> Result<RenderedPage, DocumentError> {
        only_page(index)?;
        self.draw(dpi, [0.0, 0.0, 1.0, 1.0], true)
    }

    fn render_region(
        &self,
        index: usize,
        dpi: f32,
        bbox: [f32; 4],
    ) -> Result<RenderedPage, DocumentError> {
        only_page(index)?;
        self.draw(dpi, bbox, false)
    }
}
