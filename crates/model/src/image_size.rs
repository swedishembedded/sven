// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping multimodal agent
// requests within what the serving model accepts, for its clients. If your
// team needs expertise in multimodal LLM serving then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Pixel dimensions of an image in a `data:` URL, read from the format's
//! header (PNG, JPEG, WebP, GIF) without decoding any pixels. This crate must
//! stay free of image-decoding dependencies, so the headers are read here.

use base64::{engine::general_purpose::STANDARD, Engine as _};

/// Base64 characters decoded first: enough for every header but a JPEG whose
/// metadata precedes its frame header, which gets a second look at the whole
/// payload. A multiple of 4, so the prefix decodes on its own.
const PREFIX_CHARS: usize = 64 * 1024;

/// `(width, height)` of the image in `data_url`, or `None` when it is not a
/// base64 data URL of a PNG, JPEG, WebP or GIF image with a readable header.
pub(crate) fn data_url_dimensions(data_url: &str) -> Option<(u32, u32)> {
    let (_, payload) = data_url.strip_prefix("data:")?.split_once(',')?;
    let payload = payload.trim();
    if payload.len() > PREFIX_CHARS {
        let head = STANDARD.decode(&payload[..PREFIX_CHARS]).ok()?;
        if let Some(size) = dimensions(&head) {
            return Some(size);
        }
    }
    dimensions(&STANDARD.decode(payload).ok()?)
}

fn dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let nonzero = |(w, h): (u32, u32)| (w > 0 && h > 0).then_some((w, h));
    let size = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some((be32(b, 16)?, be32(b, 20)?))
    } else if b.starts_with(b"GIF8") {
        Some((le16(b, 6)?, le16(b, 8)?))
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        webp(b)
    } else if b.starts_with(&[0xFF, 0xD8]) {
        jpeg(b)
    } else {
        None
    };
    size.and_then(nonzero)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    let t = b.get(at..at + 3)?;
    Some(u32::from(t[0]) | u32::from(t[1]) << 8 | u32::from(t[2]) << 16)
}

fn webp(b: &[u8]) -> Option<(u32, u32)> {
    match b.get(12..16)? {
        // Lossy: 14-bit sizes after the frame tag and the start code.
        b"VP8 " if b.get(23..26)? == [0x9d, 0x01, 0x2a] => {
            Some((le16(b, 26)? & 0x3fff, le16(b, 28)? & 0x3fff))
        }
        // Lossless: two 14-bit sizes, each stored minus one, packed after 0x2f.
        b"VP8L" if *b.get(20)? == 0x2f => {
            let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        // Extended: two 24-bit sizes, each stored minus one.
        b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
        _ => None,
    }
}

/// Walk the marker segments to the first frame header (SOF).
fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        if *b.get(at)? != 0xFF {
            return None;
        }
        let marker = *b.get(at + 1)?;
        match marker {
            0xFF => at += 1,               // fill byte
            0x01 | 0xD0..=0xD8 => at += 2, // standalone, no length
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let height = u32::from(u16::from_be_bytes(b.get(at + 5..at + 7)?.try_into().ok()?));
                let width = u32::from(u16::from_be_bytes(b.get(at + 7..at + 9)?.try_into().ok()?));
                return Some((width, height));
            }
            0xD9 | 0xDA => return None, // end of image / scan before any frame header
            _ => {
                let len = usize::from(u16::from_be_bytes(b.get(at + 2..at + 4)?.try_into().ok()?));
                at += 2 + len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat};

    fn url(format: ImageFormat, w: u32, h: u32) -> String {
        let mut bytes = Vec::new();
        DynamicImage::new_rgb8(w, h)
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .unwrap();
        format!("data:image/x;base64,{}", STANDARD.encode(bytes))
    }

    #[test]
    fn png_jpeg_webp_and_gif_headers_give_their_size() {
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::WebP,
            ImageFormat::Gif,
        ] {
            assert_eq!(
                data_url_dimensions(&url(format, 37, 21)),
                Some((37, 21)),
                "{format:?}"
            );
        }
    }

    #[test]
    fn a_jpeg_whose_header_lies_beyond_the_first_chunk_is_still_read() {
        // 12 comment segments (54 KB) ahead of the frame header, past the first chunk.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xFE, 0x11, 0x72];
        jpeg.extend(std::iter::repeat_n(b'x', 0x1170));
        let mut bytes = vec![0xFF, 0xD8];
        for _ in 0..12 {
            bytes.extend_from_slice(&jpeg[2..]);
        }
        bytes.extend_from_slice(&[0xFF, 0xC0, 0, 11, 8, 0, 20, 0, 30, 1, 1, 0x11, 0]);
        let url = format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes));
        assert_eq!(data_url_dimensions(&url), Some((30, 20)));
    }

    #[test]
    fn what_is_not_a_readable_image_has_no_size() {
        for url in [
            "https://example.com/a.png",
            "data:image/png;base64,aGVsbG8=",
            "data:image/png;base64,!!!",
            "data:image/png,raw",
            "data:image/png;base64,iVBORw0KGgo=",
        ] {
            assert_eq!(data_url_dimensions(url), None, "{url}");
        }
    }
}
