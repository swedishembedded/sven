// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning multimodal agent runs into training
// data then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Images in a trajectory.
//!
//! ATIF names an image by a path next to the trajectory (`ImageSourceSchema`),
//! so the trajectory stays small and the bytes are checked against their name:
//! an image is stored once as `images/<sha256 of its bytes>.<ext>`, however
//! often a conversation shows it. [`ImageSink`] turns the data URLs a model
//! was given into those references and keeps the bytes for the host to write.
//! What cannot be recorded (audio, an unsupported or malformed image) becomes
//! a text segment that says so; nothing is dropped silently.

use atif::{ContentSegment, ImageMediaType};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use sven_model::{ContentPart, ToolContentPart};

/// The directory, relative to the trajectory file, that holds its images.
pub const IMAGE_DIR: &str = "images";

/// One image a trajectory refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectoryImage {
    /// The path the trajectory names it by: `images/<sha256>.<ext>`.
    pub path: String,
    /// The image file's bytes.
    pub bytes: Vec<u8>,
}

/// Collects the images a trajectory refers to, each once.
#[derive(Debug, Default)]
pub struct ImageSink {
    images: Vec<TrajectoryImage>,
}

impl ImageSink {
    /// The segment that shows `image_url` (a data URL) in a trajectory.
    pub fn image(&mut self, image_url: &str) -> ContentSegment {
        match self.store(image_url) {
            Ok((media_type, path)) => ContentSegment::image(media_type, path),
            Err(reason) => ContentSegment::text(format!("[image not recorded: {reason}]")),
        }
    }

    /// The images collected so far, in the order each was first seen.
    #[must_use]
    pub fn into_images(self) -> Vec<TrajectoryImage> {
        self.images
    }

    /// The segments of a user message's parts.
    pub fn segments(&mut self, parts: &[ContentPart]) -> Vec<ContentSegment> {
        parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => ContentSegment::text(text.clone()),
                ContentPart::Image { image_url, .. } => self.image(image_url),
                ContentPart::Audio { .. } => audio_note(),
            })
            .collect()
    }

    /// The segments of a tool result's parts.
    pub fn tool_segments(&mut self, parts: &[ToolContentPart]) -> Vec<ContentSegment> {
        parts
            .iter()
            .map(|part| match part {
                ToolContentPart::Text { text } => ContentSegment::text(text.clone()),
                ToolContentPart::Image { image_url } => self.image(image_url),
                ToolContentPart::Audio { .. } => audio_note(),
            })
            .collect()
    }

    fn store(&mut self, image_url: &str) -> Result<(ImageMediaType, String), String> {
        let (mime, data) = sven_model::parse_data_url_parts(image_url)
            .map_err(|e| format!("{e}; only inline images are kept"))?;
        let (media_type, extension) = match mime.as_str() {
            "image/png" => (ImageMediaType::Png, "png"),
            "image/jpeg" | "image/jpg" => (ImageMediaType::Jpeg, "jpg"),
            "image/gif" => (ImageMediaType::Gif, "gif"),
            "image/webp" => (ImageMediaType::Webp, "webp"),
            other => return Err(format!("media type `{other}` is not an ATIF image type")),
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.as_bytes())
            .map_err(|e| format!("invalid base64: {e}"))?;
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let path = format!("{IMAGE_DIR}/{digest}.{extension}");
        if !self.images.iter().any(|image| image.path == path) {
            self.images.push(TrajectoryImage {
                path: path.clone(),
                bytes,
            });
        }
        Ok((media_type, path))
    }
}

fn audio_note() -> ContentSegment {
    ContentSegment::text("[audio not recorded: trajectories keep images only]")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_B64: &str = "iVBORw0KGgo=";

    fn url(mime: &str, data: &str) -> String {
        format!("data:{mime};base64,{data}")
    }

    #[test]
    fn an_image_is_named_by_its_digest_and_stored_once() {
        let mut sink = ImageSink::default();
        let a = sink.image(&url("image/png", PNG_B64));
        let b = sink.image(&url("image/png", PNG_B64));
        assert_eq!(a, b);
        let images = sink.into_images();
        assert_eq!(images.len(), 1);
        assert_eq!(
            images[0].bytes,
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
        assert!(images[0].path.starts_with("images/") && images[0].path.ends_with(".png"));
        assert_eq!(images[0].path.len(), "images/".len() + 64 + ".png".len());
    }

    #[test]
    fn what_cannot_be_recorded_is_said_in_the_step() {
        let mut sink = ImageSink::default();
        for bad in [
            "https://example.com/a.png".to_string(),
            url("image/bmp", PNG_B64),
            url("image/png", "!!not base64!!"),
        ] {
            let ContentSegment::Text { text } = sink.image(&bad) else {
                panic!("{bad} must not become an image");
            };
            assert!(text.starts_with("[image not recorded: "), "{text}");
        }
        assert!(sink.into_images().is_empty());
    }
}
