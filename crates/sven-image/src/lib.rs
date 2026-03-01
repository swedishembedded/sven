// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Image loading, resizing, and base64-encoding utilities for sven.
//!
//! This crate converts local image files into data URLs that can be embedded
//! directly in multimodal LLM API requests.
//!
//! ## Size limits
//! Images wider than [`MAX_WIDTH`] or taller than [`MAX_HEIGHT`] are downscaled
//! while preserving aspect ratio before encoding.  This keeps request payloads
//! reasonable and aligns with provider limits (most cap uploads at ~20 MB).
//!
//! ## Caching
//! [`load_image`] transparently caches encoded results in an in-process
//! LRU cache keyed on the SHA-256 of the raw file bytes.  Repeated calls with
//! the same file (or different paths to identical content) avoid the decode →
//! resize → re-encode work.  The cache holds up to [`CACHE_CAPACITY`] entries.

use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use sha2::Digest as _;

pub use error::ImageError;

mod error;

// ─── LRU image cache ──────────────────────────────────────────────────────────

/// Maximum number of encoded images held in the in-process LRU cache.
pub const CACHE_CAPACITY: usize = 32;

type CacheKey = [u8; 32]; // SHA-256 digest

fn image_cache() -> &'static Mutex<lru::LruCache<CacheKey, EncodedImage>> {
    static CACHE: OnceLock<Mutex<lru::LruCache<CacheKey, EncodedImage>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(lru::LruCache::new(
            NonZeroUsize::new(CACHE_CAPACITY).unwrap(),
        ))
    })
}

/// Maximum image width in pixels before downscaling.
pub const MAX_WIDTH: u32 = 2048;
/// Maximum image height in pixels before downscaling.
pub const MAX_HEIGHT: u32 = 2048;

/// An image that has been loaded, (optionally) resized, and encoded as base64.
#[derive(Debug, Clone)]
pub struct EncodedImage {
    /// MIME type, e.g. `"image/png"` or `"image/jpeg"`.
    pub mime_type: String,
    /// Raw bytes of the (possibly re-encoded) image.
    pub bytes: Vec<u8>,
}

impl EncodedImage {
    /// Return a data URL: `data:<mime>;base64,<b64>`.
    pub fn into_data_url(self) -> String {
        let encoded = B64.encode(&self.bytes);
        format!("data:{};base64,{}", self.mime_type, encoded)
    }

    /// Return the base64-encoded string only (no `data:…;base64,` prefix).
    pub fn to_base64(&self) -> String {
        B64.encode(&self.bytes)
    }
}

/// Load an image from `path`, resize if needed, and return an [`EncodedImage`].
///
/// Results are transparently cached by content hash: if the same image bytes
/// have been loaded before (even from a different path) the cached result is
/// returned immediately, avoiding redundant decode → resize → re-encode work.
///
/// PNG images are re-encoded as PNG; everything else is re-encoded as JPEG.
pub fn load_image(path: &Path) -> Result<EncodedImage, ImageError> {
    let raw = std::fs::read(path).map_err(|e| ImageError::Io(path.display().to_string(), e))?;

    // Content-addressed cache lookup.
    let key: CacheKey = sha2::Sha256::digest(&raw).into();
    if let Ok(mut cache) = image_cache().lock() {
        if let Some(cached) = cache.get(&key) {
            return Ok(cached.clone());
        }
    }

    let result = encode_image_bytes(&raw, path)?;

    if let Ok(mut cache) = image_cache().lock() {
        cache.put(key, result.clone());
    }

    Ok(result)
}

/// Decode, resize, and re-encode raw image bytes.
///
/// `hint_path` is used only for format detection (extension fallback) and
/// error messages; the bytes themselves are the source of truth.
fn encode_image_bytes(raw: &[u8], hint_path: &Path) -> Result<EncodedImage, ImageError> {
    // Detect format from bytes first, fall back to extension.
    let fmt = image::guess_format(raw)
        .or_else(|_| {
            let ext = hint_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            match ext.as_str() {
                "jpg" | "jpeg" => Ok(image::ImageFormat::Jpeg),
                "png" => Ok(image::ImageFormat::Png),
                "gif" => Ok(image::ImageFormat::Gif),
                "webp" => Ok(image::ImageFormat::WebP),
                _ => Err(image::ImageError::Unsupported(
                    image::error::UnsupportedError::from_format_and_kind(
                        image::error::ImageFormatHint::Unknown,
                        image::error::UnsupportedErrorKind::Format(
                            image::error::ImageFormatHint::Unknown,
                        ),
                    ),
                )),
            }
        })
        .map_err(|_| ImageError::UnsupportedFormat(hint_path.display().to_string()))?;

    let use_png = fmt == image::ImageFormat::Png;

    let img = image::load_from_memory_with_format(raw, fmt)
        .map_err(|e| ImageError::Decode(hint_path.display().to_string(), e.to_string()))?;

    let img = resize_if_needed(img);

    let mut out = Cursor::new(Vec::new());
    if use_png {
        img.write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| ImageError::Encode(e.to_string()))?;
        Ok(EncodedImage {
            mime_type: "image/png".into(),
            bytes: out.into_inner(),
        })
    } else {
        img.write_to(&mut out, image::ImageFormat::Jpeg)
            .map_err(|e| ImageError::Encode(e.to_string()))?;
        Ok(EncodedImage {
            mime_type: "image/jpeg".into(),
            bytes: out.into_inner(),
        })
    }
}

/// Parse a data URL and return `(mime_type, raw_bytes)`.
///
/// Accepts `data:<mime>;base64,<data>` format.
pub fn parse_data_url(data_url: &str) -> Result<(String, Vec<u8>), ImageError> {
    let rest = data_url
        .strip_prefix("data:")
        .ok_or_else(|| ImageError::InvalidDataUrl(data_url.to_string()))?;
    let (meta, b64) = rest
        .split_once(',')
        .ok_or_else(|| ImageError::InvalidDataUrl(data_url.to_string()))?;
    let mime = meta.strip_suffix(";base64").unwrap_or(meta).to_string();
    let bytes = B64
        .decode(b64)
        .map_err(|e| ImageError::Base64(e.to_string()))?;
    Ok((mime, bytes))
}

fn resize_if_needed(img: image::DynamicImage) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w <= MAX_WIDTH && h <= MAX_HEIGHT {
        return img;
    }
    let ratio_w = MAX_WIDTH as f64 / w as f64;
    let ratio_h = MAX_HEIGHT as f64 / h as f64;
    let ratio = ratio_w.min(ratio_h);
    let new_w = ((w as f64 * ratio).round() as u32).max(1);
    let new_h = ((h as f64 * ratio).round() as u32).max(1);
    img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
}

/// Return whether the given file extension belongs to a supported image format.
pub fn is_image_extension(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_data_url_valid() {
        let url = "data:image/png;base64,aGVsbG8=";
        let (mime, bytes) = parse_data_url(url).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn parse_data_url_invalid_prefix() {
        assert!(parse_data_url("not-a-data-url").is_err());
    }

    #[test]
    fn is_image_extension_recognises_known_formats() {
        for ext in &["png", "PNG", "jpg", "jpeg", "gif", "webp", "bmp", "tiff"] {
            assert!(is_image_extension(ext), "{ext} should be recognised");
        }
    }

    #[test]
    fn is_image_extension_rejects_unknown() {
        assert!(!is_image_extension("rs"));
        assert!(!is_image_extension("txt"));
        assert!(!is_image_extension(""));
    }

    #[test]
    fn encoded_image_into_data_url() {
        let img = EncodedImage {
            mime_type: "image/png".into(),
            bytes: b"rawbytes".to_vec(),
        };
        let url = img.into_data_url();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    // 1×1 red PNG bytes (valid minimal PNG, CRCs verified by Python zlib)
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
        0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1×1
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, // bit depth 8, RGB
        0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, // IDAT length + "IDAT"
        0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00, // compressed pixel (red)
        0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, // IDAT CRC
        0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, // IEND
        0x44, 0xae, 0x42, 0x60, 0x82, // IEND CRC
    ];

    #[test]
    fn load_minimal_png() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), MINIMAL_PNG).unwrap();
        let result = load_image(tmp.path());
        assert!(
            result.is_ok(),
            "should load minimal PNG: {:?}",
            result.err()
        );
        let img = result.unwrap();
        assert_eq!(img.mime_type, "image/png");
        assert!(!img.bytes.is_empty());
    }

    #[test]
    fn load_image_returns_same_bytes_on_second_call() {
        // Second call for identical content should produce bitwise-identical output
        // (from cache) without re-encoding.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), MINIMAL_PNG).unwrap();

        let first = load_image(tmp.path()).unwrap();
        let second = load_image(tmp.path()).unwrap();

        assert_eq!(first.mime_type, second.mime_type);
        assert_eq!(
            first.bytes, second.bytes,
            "second call should return cached bytes identical to first call"
        );
    }

    #[test]
    fn identical_content_at_different_paths_shares_cache_entry() {
        // Two files with identical bytes → same cache entry.
        let tmp1 = tempfile::NamedTempFile::new().unwrap();
        let tmp2 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp1.path(), MINIMAL_PNG).unwrap();
        std::fs::write(tmp2.path(), MINIMAL_PNG).unwrap();

        let a = load_image(tmp1.path()).unwrap();
        let b = load_image(tmp2.path()).unwrap();

        assert_eq!(
            a.bytes, b.bytes,
            "identical content from different paths should yield identical encoded output"
        );
    }
}
