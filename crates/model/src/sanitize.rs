// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Message sanitization: strip image or audio content when the model does not
//! support it.
//!
//! Call [`strip_images_if_unsupported`] and [`strip_audio_if_unsupported`]
//! before building a [`crate::CompletionRequest`] to ensure that image / audio parts
//! are replaced with a text placeholder whenever the target model cannot
//! accept them.

use crate::{
    catalog::{ImageLimits, InputModality},
    types::{ContentPart, Message, MessageContent, ToolContentPart, ToolResultContent},
};

const IMAGE_OMITTED: &str = "[image omitted: model does not support image input]";
const AUDIO_OMITTED: &str = "[audio omitted: model does not support audio input]";

/// Replace all image content in `messages` with a text placeholder when
/// `modalities` does not include [`InputModality::Image`].
///
/// If the model *does* support images this is a no-op and the messages are
/// returned unchanged.
pub fn strip_images_if_unsupported(
    messages: Vec<Message>,
    modalities: &[InputModality],
) -> Vec<Message> {
    strip_if_unsupported(messages, modalities, InputModality::Image, IMAGE_OMITTED)
}

/// Replace all audio content in `messages` with a text placeholder when
/// `modalities` does not include [`InputModality::Audio`].
///
/// If the model *does* accept audio this is a no-op and the messages are
/// returned unchanged.  Mirrors [`strip_images_if_unsupported`].
pub fn strip_audio_if_unsupported(
    messages: Vec<Message>,
    modalities: &[InputModality],
) -> Vec<Message> {
    strip_if_unsupported(messages, modalities, InputModality::Audio, AUDIO_OMITTED)
}

/// Shared implementation for the per-modality strippers.
fn strip_if_unsupported(
    messages: Vec<Message>,
    modalities: &[InputModality],
    target: InputModality,
    placeholder: &str,
) -> Vec<Message> {
    if modalities.contains(&target) {
        return messages;
    }
    rewrite_parts(messages, target, &mut |_| Some(placeholder.to_string()))
}

/// Per-request image limits applied to the outgoing request.
///
/// When the images of `messages` exceed `limits` (their number, or the pixels
/// summed over all of them), the OLDEST images are replaced in place by a
/// short text note and the newest are kept: the newest are the ones the
/// current step needs. Newest first, an image is kept while the images kept
/// so far plus this one fit; the first image that does not fit and every
/// older one are left out. An image whose size cannot be read (a remote URL,
/// unreadable bytes) counts as over a pixel limit, because a request that may
/// be refused is never sent; its note says so.
///
/// Unknown limits change nothing. The caller's conversation is untouched:
/// this works on the copy that becomes the request.
pub fn limit_images(messages: Vec<Message>, limits: ImageLimits) -> Vec<Message> {
    if !limits.is_known() {
        return messages;
    }
    let mut urls = Vec::new();
    let messages = rewrite_parts(messages, InputModality::Image, &mut |url| {
        urls.push(url.to_string());
        None
    });
    let notes = plan_notes(&urls, limits);
    if notes.iter().all(Option::is_none) {
        return messages;
    }
    let mut notes = notes.into_iter();
    rewrite_parts(messages, InputModality::Image, &mut |_| {
        notes.next().flatten()
    })
}

/// Why an image does not fit.
#[derive(Clone, Copy)]
enum Excess {
    Count(u32),
    Pixels(u64),
    SizeUnreadable(u64),
}

/// For each image, oldest first: the note replacing it, or `None` to keep it.
fn plan_notes(urls: &[String], limits: ImageLimits) -> Vec<Option<String>> {
    let sizes: Vec<Option<(u32, u32)>> = urls
        .iter()
        .map(|u| {
            // Only a pixel limit needs the sizes; the notes name them when known.
            limits
                .max_pixels
                .and_then(|_| crate::image_size::data_url_dimensions(u))
        })
        .collect();
    let mut notes = vec![None; urls.len()];
    let (mut kept, mut kept_pixels) = (0u32, 0u64);
    let mut stopped_by: Option<Excess> = None;
    for i in (0..urls.len()).rev() {
        if stopped_by.is_none() {
            stopped_by = excess(limits, kept, kept_pixels, sizes[i]);
            if stopped_by.is_none() {
                kept += 1;
                kept_pixels += sizes[i].map_or(0, |(w, h)| u64::from(w) * u64::from(h));
            }
        }
        if let Some(reason) = stopped_by {
            notes[i] = Some(omission_note(&urls[i], sizes[i], reason));
        }
    }
    notes
}

fn excess(
    limits: ImageLimits,
    kept: u32,
    kept_pixels: u64,
    size: Option<(u32, u32)>,
) -> Option<Excess> {
    if let Some(max) = limits.max_images.filter(|&max| kept >= max) {
        return Some(Excess::Count(max));
    }
    let max = limits.max_pixels?;
    match size {
        None => Some(Excess::SizeUnreadable(max)),
        Some((w, h)) => {
            (kept_pixels + u64::from(w) * u64::from(h) > max).then_some(Excess::Pixels(max))
        }
    }
}

fn omission_note(url: &str, size: Option<(u32, u32)>, reason: Excess) -> String {
    let mime = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split([';', ',']).next())
        .filter(|m| !m.is_empty())
        .unwrap_or("remote image");
    let what = match size {
        Some((w, h)) => format!("{mime}, {w}x{h}"),
        None => mime.to_string(),
    };
    let why = match reason {
        Excess::Count(n) => format!("the server takes at most {n} images per request"),
        Excess::Pixels(p) => {
            format!("the server takes at most {p} pixels of images per request")
        }
        Excess::SizeUnreadable(p) => format!(
            "its size could not be read and the server takes at most {p} pixels of images per request"
        ),
    };
    format!("[earlier image left out: {what}, {why}]")
}

/// The URL of `part` when it carries content of the modality `target`.
fn part_url(part: &ContentPart, target: InputModality) -> Option<&str> {
    match (part, target) {
        (ContentPart::Image { image_url, .. }, InputModality::Image) => Some(image_url),
        (ContentPart::Audio { audio_url, .. }, InputModality::Audio) => Some(audio_url),
        _ => None,
    }
}

/// The URL of `part` when it carries content of the modality `target`.
fn tool_part_url(part: &ToolContentPart, target: InputModality) -> Option<&str> {
    match (part, target) {
        (ToolContentPart::Image { image_url }, InputModality::Image) => Some(image_url),
        (ToolContentPart::Audio { audio_url }, InputModality::Audio) => Some(audio_url),
        _ => None,
    }
}

/// Visit every part of modality `target` in message order (user parts and
/// tool-result parts alike); where `replace` returns text, that text takes
/// the part's place.
fn rewrite_parts(
    messages: Vec<Message>,
    target: InputModality,
    replace: &mut dyn FnMut(&str) -> Option<String>,
) -> Vec<Message> {
    messages
        .into_iter()
        .map(|m| rewrite_message(m, target, replace))
        .collect()
}

fn rewrite_message(
    mut m: Message,
    target: InputModality,
    replace: &mut dyn FnMut(&str) -> Option<String>,
) -> Message {
    m.content = match m.content {
        MessageContent::ContentParts(parts) => {
            let rewritten: Vec<ContentPart> = parts
                .into_iter()
                .map(|p| match part_url(&p, target).and_then(&mut *replace) {
                    Some(text) => ContentPart::Text { text },
                    None => p,
                })
                .collect();
            // Collapse single text part back to Text for cleaner serialization.
            if let [ContentPart::Text { text }] = rewritten.as_slice() {
                return Message {
                    content: MessageContent::Text(text.clone()),
                    ..m
                };
            }
            MessageContent::ContentParts(rewritten)
        }
        MessageContent::ToolResult {
            tool_call_id,
            content: ToolResultContent::Parts(parts),
        } => {
            let rewritten: Vec<ToolContentPart> = parts
                .into_iter()
                .map(
                    |p| match tool_part_url(&p, target).and_then(&mut *replace) {
                        Some(text) => ToolContentPart::Text { text },
                        None => p,
                    },
                )
                .collect();
            // Collapse single text part back to Text.
            let content = match rewritten.as_slice() {
                [ToolContentPart::Text { text }] => ToolResultContent::Text(text.clone()),
                _ => ToolResultContent::Parts(rewritten),
            };
            MessageContent::ToolResult {
                tool_call_id,
                content,
            }
        }
        other => other,
    };
    m
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentPart, ToolContentPart};

    fn text_only_modalities() -> Vec<InputModality> {
        vec![InputModality::Text]
    }

    fn vision_modalities() -> Vec<InputModality> {
        vec![InputModality::Text, InputModality::Image]
    }

    #[test]
    fn no_op_when_image_supported() {
        let msg = Message::user_with_parts(vec![
            ContentPart::Text {
                text: "hello".into(),
            },
            ContentPart::image("data:image/png;base64,ABC"),
        ]);
        let result = strip_images_if_unsupported(vec![msg], &vision_modalities());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].image_urls(), vec!["data:image/png;base64,ABC"]);
    }

    #[test]
    fn strips_image_parts_from_content_parts() {
        let msg = Message::user_with_parts(vec![
            ContentPart::Text {
                text: "describe this".into(),
            },
            ContentPart::image("data:image/png;base64,ABC"),
        ]);
        let result = strip_images_if_unsupported(vec![msg], &text_only_modalities());
        assert_eq!(result.len(), 1);
        assert!(result[0].image_urls().is_empty());
        match &result[0].content {
            MessageContent::ContentParts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[1], ContentPart::Text { text } if text == IMAGE_OMITTED));
            }
            other => panic!("expected ContentParts, got {:?}", other),
        }
    }

    #[test]
    fn strips_image_from_single_part_collapses_to_text() {
        let msg = Message::user_with_parts(vec![ContentPart::image("data:image/png;base64,ABC")]);
        let result = strip_images_if_unsupported(vec![msg], &text_only_modalities());
        assert!(matches!(result[0].content, MessageContent::Text(_)));
        assert_eq!(result[0].as_text(), Some(IMAGE_OMITTED));
    }

    #[test]
    fn strips_image_from_tool_result_parts() {
        let parts = vec![
            ToolContentPart::Text {
                text: "result".into(),
            },
            ToolContentPart::Image {
                image_url: "data:image/png;base64,XYZ".into(),
            },
        ];
        let msg = Message::tool_result_with_parts("id-1", parts);
        let result = strip_images_if_unsupported(vec![msg], &text_only_modalities());
        match &result[0].content {
            MessageContent::ToolResult { content, .. } => {
                assert!(content.image_urls().is_empty());
                match content {
                    ToolResultContent::Parts(p) => {
                        assert!(
                            matches!(&p[1], ToolContentPart::Text { text } if text == IMAGE_OMITTED)
                        );
                    }
                    other => panic!("expected Parts, got {:?}", other),
                }
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn strips_single_image_tool_result_collapses_to_text() {
        let parts = vec![ToolContentPart::Image {
            image_url: "data:image/png;base64,XYZ".into(),
        }];
        let msg = Message::tool_result_with_parts("id-1", parts);
        let result = strip_images_if_unsupported(vec![msg], &text_only_modalities());
        match &result[0].content {
            MessageContent::ToolResult { content, .. } => {
                assert!(matches!(content, ToolResultContent::Text(t) if t == IMAGE_OMITTED));
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    // ── Audio ─────────────────────────────────────────────────────────────────

    fn audio_modalities() -> Vec<InputModality> {
        vec![InputModality::Text, InputModality::Audio]
    }

    #[test]
    fn no_op_when_audio_supported() {
        let msg = Message::user_with_parts(vec![
            ContentPart::Text {
                text: "listen".into(),
            },
            ContentPart::audio("data:audio/wav;base64,ABC"),
        ]);
        let result = strip_audio_if_unsupported(vec![msg], &audio_modalities());
        assert_eq!(result[0].audio_urls(), vec!["data:audio/wav;base64,ABC"]);
    }

    #[test]
    fn strips_audio_parts_from_content_parts() {
        let msg = Message::user_with_parts(vec![
            ContentPart::Text {
                text: "transcribe this".into(),
            },
            ContentPart::audio("data:audio/wav;base64,ABC"),
        ]);
        let result = strip_audio_if_unsupported(vec![msg], &text_only_modalities());
        assert!(result[0].audio_urls().is_empty());
        match &result[0].content {
            MessageContent::ContentParts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[1], ContentPart::Text { text } if text == AUDIO_OMITTED));
            }
            other => panic!("expected ContentParts, got {:?}", other),
        }
    }

    #[test]
    fn strips_audio_from_single_part_collapses_to_text() {
        let msg = Message::user_with_parts(vec![ContentPart::audio("data:audio/wav;base64,ABC")]);
        let result = strip_audio_if_unsupported(vec![msg], &text_only_modalities());
        assert!(matches!(result[0].content, MessageContent::Text(_)));
        assert_eq!(result[0].as_text(), Some(AUDIO_OMITTED));
    }

    #[test]
    fn strips_audio_from_tool_result_parts() {
        let parts = vec![
            ToolContentPart::Text {
                text: "result".into(),
            },
            ToolContentPart::Audio {
                audio_url: "data:audio/wav;base64,XYZ".into(),
            },
        ];
        let msg = Message::tool_result_with_parts("id-1", parts);
        let result = strip_audio_if_unsupported(vec![msg], &text_only_modalities());
        match &result[0].content {
            MessageContent::ToolResult { content, .. } => {
                assert!(content.audio_urls().is_empty());
                match content {
                    ToolResultContent::Parts(p) => {
                        assert!(
                            matches!(&p[1], ToolContentPart::Text { text } if text == AUDIO_OMITTED)
                        );
                    }
                    other => panic!("expected Parts, got {:?}", other),
                }
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    /// Stripping audio must leave image parts untouched, and vice versa.
    #[test]
    fn audio_strip_leaves_images_intact() {
        let msg = Message::user_with_parts(vec![
            ContentPart::image("data:image/png;base64,IMG"),
            ContentPart::audio("data:audio/wav;base64,AUD"),
        ]);
        let result = strip_audio_if_unsupported(vec![msg], &vision_modalities());
        assert_eq!(result[0].image_urls(), vec!["data:image/png;base64,IMG"]);
        assert!(result[0].audio_urls().is_empty());
    }

    #[test]
    fn image_strip_leaves_audio_intact() {
        let msg = Message::user_with_parts(vec![
            ContentPart::image("data:image/png;base64,IMG"),
            ContentPart::audio("data:audio/wav;base64,AUD"),
        ]);
        let result = strip_images_if_unsupported(vec![msg], &audio_modalities());
        assert!(result[0].image_urls().is_empty());
        assert_eq!(result[0].audio_urls(), vec!["data:audio/wav;base64,AUD"]);
    }

    #[test]
    fn plain_text_messages_pass_through_unchanged() {
        let msgs = vec![Message::user("hello"), Message::assistant("world")];
        let result = strip_images_if_unsupported(msgs, &text_only_modalities());
        assert_eq!(result[0].as_text(), Some("hello"));
        assert_eq!(result[1].as_text(), Some("world"));
    }

    // ── Per-request image limits ──────────────────────────────────────────────

    fn png_url(w: u32, h: u32) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(w, h)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        format!("data:image/png;base64,{}", STANDARD.encode(bytes))
    }

    fn images_msg(urls: &[String]) -> Message {
        Message::user_with_parts(urls.iter().map(|u| ContentPart::image(u.clone())).collect())
    }

    fn kept_urls(msgs: &[Message]) -> Vec<String> {
        msgs.iter()
            .flat_map(|m| m.image_urls())
            .map(str::to_string)
            .collect()
    }

    fn notes_in(msgs: &[Message]) -> Vec<String> {
        msgs.iter()
            .flat_map(|m| match &m.content {
                MessageContent::ContentParts(p) => p
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } if text.starts_with("[earlier image") => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                MessageContent::Text(t) if t.starts_with("[earlier image") => vec![t.clone()],
                MessageContent::ToolResult {
                    content: ToolResultContent::Parts(p),
                    ..
                } => p
                    .iter()
                    .filter_map(|p| match p {
                        ToolContentPart::Text { text } if text.starts_with("[earlier image") => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            })
            .collect()
    }

    #[test]
    fn the_oldest_images_beyond_the_count_limit_become_notes() {
        let urls: Vec<String> = (1..=4).map(|n| png_url(n, 1)).collect();
        let msgs = vec![
            images_msg(&urls[..2]),
            Message::tool_result_with_parts(
                "t",
                vec![
                    ToolContentPart::Image {
                        image_url: urls[2].clone(),
                    },
                    ToolContentPart::Image {
                        image_url: urls[3].clone(),
                    },
                ],
            ),
        ];
        let limits = ImageLimits {
            max_images: Some(2),
            max_pixels: None,
        };
        let out = limit_images(msgs.clone(), limits);
        assert_eq!(kept_urls(&out), urls[2..].to_vec(), "the newest two stay");
        let notes = notes_in(&out);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|n| n.contains("image/png")), "{notes:?}");
        assert!(notes
            .iter()
            .all(|n| n.contains("at most 2 images per request")));
        assert_eq!(
            kept_urls(&msgs).len(),
            4,
            "the input conversation is not edited"
        );
    }

    #[test]
    fn images_within_the_limits_or_with_unknown_limits_are_untouched() {
        let urls: Vec<String> = (1..=3).map(|n| png_url(n, 1)).collect();
        for limits in [
            ImageLimits::default(),
            ImageLimits {
                max_images: Some(3),
                max_pixels: Some(100),
            },
        ] {
            let out = limit_images(vec![images_msg(&urls)], limits);
            assert_eq!(kept_urls(&out), urls);
            assert!(notes_in(&out).is_empty());
        }
    }

    #[test]
    fn the_pixel_total_keeps_the_newest_images_that_fit() {
        // 10x10 = 100 pixels each; 250 allows two.
        let urls: Vec<String> = (0..3).map(|_| png_url(10, 10)).collect();
        let limits = ImageLimits {
            max_images: None,
            max_pixels: Some(250),
        };
        let out = limit_images(vec![images_msg(&urls)], limits);
        assert_eq!(kept_urls(&out).len(), 2);
        let notes = notes_in(&out);
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("10x10") && notes[0].contains("250 pixels"),
            "{notes:?}"
        );
    }

    #[test]
    fn an_image_of_unreadable_size_counts_as_over_a_pixel_limit() {
        let urls = vec!["https://example.com/a.png".to_string(), png_url(2, 2)];
        let limits = ImageLimits {
            max_images: None,
            max_pixels: Some(1_000_000),
        };
        let out = limit_images(vec![images_msg(&urls)], limits);
        assert_eq!(kept_urls(&out), vec![urls[1].clone()]);
        assert!(notes_in(&out)[0].contains("size could not be read"));
    }

    #[test]
    fn a_single_left_out_image_collapses_to_plain_text() {
        let urls = [png_url(1, 1), png_url(1, 1)];
        let msgs = vec![images_msg(&urls[..1]), images_msg(&urls[1..])];
        let limits = ImageLimits {
            max_images: Some(1),
            max_pixels: None,
        };
        let out = limit_images(msgs, limits);
        assert!(matches!(out[0].content, MessageContent::Text(_)));
        assert_eq!(out[1].image_urls().len(), 1);
    }
}
