// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements multimodal agents that stay cheap to run on
// a slow link for its clients. If your team needs expertise in image
// handling for LLM requests then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The size an image is given to a model: as large as the model looks at it,
//! and no larger.
//!
//! Providers scale every image down to the size their model reads before it
//! is read: Claude to a long edge of 2576 px on its high-resolution models
//! (1568 px on the others), OpenAI's high detail to 2048 px. Bytes beyond the
//! largest of those are sent, and resent with every later request of the
//! conversation, for nothing. So every image that enters a conversation is
//! fitted once to [`MODEL_LONG_EDGE`], the largest, so no model sees less
//! than it would, and encoded small. A
//! screenshot, a diagram or a drawing (few colours) stays PNG, so its text
//! stays sharp, unless even that is over [`PNG_KEPT_UNDER`]; a photo or a
//! rendering (many colours) becomes JPEG. The
//! original file is never touched; a model that needs a detail looks at the
//! file again, or a crop of it.

use std::io::Cursor;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

use crate::{parse_data_url, EncodedImage, ImageError};

/// The longest edge, in pixels, an image is given to a model: the largest a
/// current model reads (Claude's high-resolution models), so none is given
/// less than it would look at; others scale it down themselves.
pub const MODEL_LONG_EDGE: u32 = 2576;

/// A screenshot or diagram whose PNG stays under this many bytes stays PNG;
/// over it, it becomes a high-quality JPEG. At the model's size a full
/// screen of text is about 1.6 MB as PNG, under half again its JPEG, and
/// lossless text is what a model reads most closely.
pub const PNG_KEPT_UNDER: usize = 5 * 512 * 1024;

/// The quality of a JPEG a photo or a rendering becomes.
const PHOTO_QUALITY: u8 = 85;

/// The quality of a JPEG a screenshot or diagram too large as PNG becomes:
/// high, so its text stays legible.
const GRAPHIC_QUALITY: u8 = 92;

/// Fewer distinct colours than this, in a sample of the image, make it a
/// screenshot, a diagram or a drawing rather than a photo.
const GRAPHIC_COLOURS: usize = 2048;

/// `img` fitted to [`MODEL_LONG_EDGE`] and encoded as this module says.
///
/// # Errors
/// The image cannot be encoded.
pub fn encode_for_model(img: image::DynamicImage) -> Result<EncodedImage, ImageError> {
    let graphic = is_graphic(&img);
    let img = fit_long_edge(img, MODEL_LONG_EDGE);
    if graphic {
        let png = encode_png(&img)?;
        if png.len() <= PNG_KEPT_UNDER {
            return Ok(EncodedImage {
                mime_type: "image/png".into(),
                bytes: png,
            });
        }
        return encode_jpeg(&img, GRAPHIC_QUALITY);
    }
    encode_jpeg(&img, PHOTO_QUALITY)
}

fn encode_png(img: &image::DynamicImage) -> Result<Vec<u8>, ImageError> {
    let mut png = Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| ImageError::Encode(e.to_string()))?;
    Ok(png.into_inner())
}

fn encode_jpeg(img: &image::DynamicImage, quality: u8) -> Result<EncodedImage, ImageError> {
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
        .encode_image(&image::DynamicImage::ImageRgb8(img.to_rgb8()))
        .map_err(|e| ImageError::Encode(e.to_string()))?;
    Ok(EncodedImage {
        mime_type: "image/jpeg".into(),
        bytes: jpeg,
    })
}

/// Whether `img` is a screenshot, a diagram or a drawing: few distinct
/// colours over an even sample of at most 65536 of its pixels.
fn is_graphic(img: &image::DynamicImage) -> bool {
    let rgb = img.to_rgb8();
    let pixels = u64::from(rgb.width()) * u64::from(rgb.height());
    let step = (pixels / 65_536).max(1) as usize;
    let mut seen = std::collections::HashSet::new();
    for pixel in rgb.pixels().step_by(step) {
        seen.insert(pixel.0);
        if seen.len() >= GRAPHIC_COLOURS {
            return false;
        }
    }
    true
}

/// The data URL `data_url` becomes for a model, or `None` when it stays as it
/// is: already within the size, not an image this crate reads, or not a data
/// URL at all (a remote image is the server's to fetch).
#[must_use]
pub fn for_model(data_url: &str) -> Option<String> {
    let (mime, bytes) = parse_data_url(data_url).ok()?;
    if !mime.starts_with("image/") {
        return None;
    }
    let img = image::load_from_memory(&bytes).ok()?;
    let fits = img.width().max(img.height()) <= MODEL_LONG_EDGE;
    if fits && bytes.len() <= PNG_KEPT_UNDER {
        return None;
    }
    let encoded = encode_for_model(img).ok()?;
    if fits && encoded.bytes.len() >= bytes.len() {
        return None;
    }
    Some(format!(
        "data:{};base64,{}",
        encoded.mime_type,
        B64.encode(&encoded.bytes)
    ))
}

/// `img` scaled down, keeping its proportions, so its longest edge is at most
/// `long_edge`; never scaled up.
fn fit_long_edge(img: image::DynamicImage, long_edge: u32) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= long_edge {
        return img;
    }
    let ratio = f64::from(long_edge) / f64::from(w.max(h));
    let new_w = ((f64::from(w) * ratio).round() as u32).max(1);
    let new_h = ((f64::from(h) * ratio).round() as u32).max(1);
    img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_url(img: &image::DynamicImage, format: image::ImageFormat, mime: &str) -> String {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        format!("data:{mime};base64,{}", B64.encode(out.into_inner()))
    }

    fn decoded(url: &str) -> (String, image::DynamicImage) {
        let (mime, bytes) = parse_data_url(url).unwrap();
        (mime, image::load_from_memory(&bytes).unwrap())
    }

    /// A photo or rendering: every pixel different, as noise is.
    fn photo(w: u32, h: u32) -> image::DynamicImage {
        let mut state = 0x2545_f491_u32;
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [r, g, b, _] = state.to_le_bytes();
            image::Rgb([r, g, b])
        }))
    }

    /// A screenshot of a large screen: a sidebar, a toolbar, flat panels
    /// and lines of text.
    fn screenshot(w: u32, h: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
            let text = (y % 48 < 14) && (x % 14 < 9) && (y / 48) % 5 != 4;
            if y < 90 {
                image::Rgb([40, 44, 52])
            } else if x < w / 5 {
                if text && x > 30 && x < w / 5 - 60 {
                    image::Rgb([200, 200, 200])
                } else {
                    image::Rgb([33, 37, 43])
                }
            } else if text && x > w / 5 + 60 && x < w - 200 {
                image::Rgb([30, 30, 30])
            } else {
                image::Rgb([250, 250, 250])
            }
        }))
    }

    #[test]
    fn a_large_photo_is_fitted_to_the_long_edge_and_becomes_a_small_jpeg() {
        let original = data_url(&photo(3900, 2600), image::ImageFormat::Png, "image/png");
        let fitted = for_model(&original).expect("resized");
        let (mime, img) = decoded(&fitted);
        assert_eq!(mime, "image/jpeg");
        assert_eq!((img.width(), img.height()), (MODEL_LONG_EDGE, 1717));
        assert!(
            fitted.len() < original.len() / 4,
            "{} -> {}",
            original.len(),
            fitted.len()
        );
    }

    #[test]
    fn a_large_screenshot_is_fitted_and_stays_a_lossless_png() {
        let original = data_url(
            &screenshot(3840, 2400),
            image::ImageFormat::Png,
            "image/png",
        );
        let fitted = for_model(&original).expect("resized");
        let (mime, img) = decoded(&fitted);
        assert_eq!(mime, "image/png", "text stays sharp");
        assert_eq!((img.width(), img.height()), (MODEL_LONG_EDGE, 1610));
    }

    #[test]
    fn an_image_already_within_the_size_is_left_as_it_is() {
        let small = data_url(&screenshot(800, 600), image::ImageFormat::Png, "image/png");
        assert_eq!(for_model(&small), None);
    }

    #[test]
    fn what_is_not_an_image_data_url_is_left_alone() {
        assert_eq!(for_model("https://example.com/a.png"), None);
        assert_eq!(for_model("data:audio/wav;base64,UklGRg=="), None);
        assert_eq!(for_model("data:image/png;base64,bm90IGFuIGltYWdl"), None);
    }
}
