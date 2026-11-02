// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The images a tool returned, in the form a session event carries them.
//!
//! Swedish Embedded AB implements solutions for showing agent work to people
//! on remote surfaces for its clients. If your team needs expertise in
//! remote-controlled agent clients then you can procure our services by
//! sending an email to info@swedishembedded.com.
//!
//! A tool result reaches the model with its images; a client watching the
//! session event stream gets them as [`ToolImage`]s on
//! `SessionEvent::ToolCallFinished`. Only raster images that arrive as base64
//! data URLs are carried, and the payload is bounded so one result cannot
//! make an unbounded event.

use crate::ToolOutputPart;

/// Most images carried with one tool result.
pub const MAX_IMAGES: usize = 4;

/// Largest base64 payload (bytes of text) of one carried image.
pub const MAX_IMAGE_BASE64_BYTES: usize = 4 * 1024 * 1024;

/// Text appended to a result's output when an image was left out for size.
pub const TOO_LARGE_NOTE: &str = "(an image was too large to show)";

/// Text appended to a result's output when images beyond [`MAX_IMAGES`] were left out.
pub const TOO_MANY_NOTE: &str = "(more images than can be shown were left out)";

const RASTER_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// One image a tool returned: its media type and base64 data (no `data:`
/// prefix).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolImage {
    pub media_type: String,
    pub data: String,
}

/// The images of a tool result that fit the bounds, and the notes for what
/// was left out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShownImages {
    pub images: Vec<ToolImage>,
    /// Empty when nothing carried was left out; otherwise text to append to
    /// the result's output so a reader knows an image is missing.
    pub note: String,
}

impl ShownImages {
    /// The images among `parts` that can be shown, bounded by [`MAX_IMAGES`]
    /// and [`MAX_IMAGE_BASE64_BYTES`]. Non-raster media, remote URLs and
    /// malformed data URLs are skipped silently: they are not images this
    /// stream carries.
    pub fn from_parts(parts: &[ToolOutputPart]) -> Self {
        let mut shown = Self::default();
        let (mut too_large, mut too_many) = (false, false);
        for part in parts {
            let ToolOutputPart::Image(url) = part else {
                continue;
            };
            let Some((media_type, data)) = raster_data_url(url) else {
                continue;
            };
            if data.len() > MAX_IMAGE_BASE64_BYTES {
                too_large = true;
            } else if shown.images.len() >= MAX_IMAGES {
                too_many = true;
            } else {
                shown.images.push(ToolImage {
                    media_type: media_type.to_string(),
                    data: data.to_string(),
                });
            }
        }
        for (left_out, note) in [(too_large, TOO_LARGE_NOTE), (too_many, TOO_MANY_NOTE)] {
            if left_out {
                if !shown.note.is_empty() {
                    shown.note.push('\n');
                }
                shown.note.push_str(note);
            }
        }
        shown
    }
}

/// `(media type, base64)` of a `data:<raster>;base64,<data>` URL.
fn raster_data_url(url: &str) -> Option<(&str, &str)> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    RASTER_TYPES
        .iter()
        .find(|known| known.eq_ignore_ascii_case(media_type))
        .map(|known| (*known, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(url: &str) -> ToolOutputPart {
        ToolOutputPart::Image(url.to_string())
    }

    #[test]
    fn carries_raster_data_urls_without_the_prefix() {
        let shown = ShownImages::from_parts(&[
            ToolOutputPart::Text("Image file: a.png".into()),
            image("data:image/png;base64,iVBO"),
            image("data:image/webp;base64,UklG"),
        ]);
        assert_eq!(
            shown.images,
            vec![
                ToolImage {
                    media_type: "image/png".into(),
                    data: "iVBO".into()
                },
                ToolImage {
                    media_type: "image/webp".into(),
                    data: "UklG".into()
                },
            ]
        );
        assert_eq!(shown.note, "");
    }

    #[test]
    fn skips_what_is_not_a_base64_raster_data_url() {
        let shown = ShownImages::from_parts(&[
            image("https://example.com/a.png"),
            image("data:image/svg+xml;base64,PHN2Zz4="),
            image("data:image/png,rawbytes"),
            image("data:audio/wav;base64,UklG"),
            ToolOutputPart::Audio("data:audio/wav;base64,UklG".into()),
        ]);
        assert_eq!(shown, ShownImages::default());
    }

    #[test]
    fn leaves_out_an_oversized_image_and_says_so() {
        let big = format!(
            "data:image/png;base64,{}",
            "A".repeat(MAX_IMAGE_BASE64_BYTES + 1)
        );
        let at_limit = format!(
            "data:image/png;base64,{}",
            "A".repeat(MAX_IMAGE_BASE64_BYTES)
        );
        let shown = ShownImages::from_parts(&[image(&big), image(&at_limit)]);
        assert_eq!(shown.images.len(), 1);
        assert_eq!(shown.images[0].data.len(), MAX_IMAGE_BASE64_BYTES);
        assert_eq!(shown.note, TOO_LARGE_NOTE);
    }

    #[test]
    fn carries_at_most_eight_images() {
        let parts: Vec<_> = (0..MAX_IMAGES + 2)
            .map(|i| image(&format!("data:image/gif;base64,R{i}")))
            .collect();
        let shown = ShownImages::from_parts(&parts);
        assert_eq!(shown.images.len(), MAX_IMAGES);
        assert_eq!(shown.images[MAX_IMAGES - 1].data, "R3");
        assert_eq!(shown.note, TOO_MANY_NOTE);
    }
}
