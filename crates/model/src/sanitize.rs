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
    catalog::InputModality,
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
    messages
        .into_iter()
        .map(|m| strip_message(m, target, placeholder))
        .collect()
}

/// Return `true` when `part` carries content of the modality being stripped.
fn part_matches(part: &ContentPart, target: InputModality) -> bool {
    matches!(
        (part, target),
        (ContentPart::Image { .. }, InputModality::Image)
            | (ContentPart::Audio { .. }, InputModality::Audio)
    )
}

/// Return `true` when `part` carries content of the modality being stripped.
fn tool_part_matches(part: &ToolContentPart, target: InputModality) -> bool {
    matches!(
        (part, target),
        (ToolContentPart::Image { .. }, InputModality::Image)
            | (ToolContentPart::Audio { .. }, InputModality::Audio)
    )
}

fn strip_message(mut m: Message, target: InputModality, placeholder: &str) -> Message {
    m.content = match m.content {
        MessageContent::ContentParts(parts) => {
            let stripped: Vec<ContentPart> = parts
                .into_iter()
                .map(|p| {
                    if part_matches(&p, target) {
                        ContentPart::Text {
                            text: placeholder.to_string(),
                        }
                    } else {
                        p
                    }
                })
                .collect();
            // Collapse single text part back to Text for cleaner serialization.
            if stripped.len() == 1 {
                if let ContentPart::Text { text } = &stripped[0] {
                    return Message {
                        content: MessageContent::Text(text.clone()),
                        ..m
                    };
                }
            }
            MessageContent::ContentParts(stripped)
        }
        MessageContent::ToolResult {
            tool_call_id,
            content,
        } => {
            let content = strip_tool_result_content(content, target, placeholder);
            MessageContent::ToolResult {
                tool_call_id,
                content,
            }
        }
        other => other,
    };
    m
}

fn strip_tool_result_content(
    content: ToolResultContent,
    target: InputModality,
    placeholder: &str,
) -> ToolResultContent {
    match content {
        ToolResultContent::Parts(parts) => {
            let stripped: Vec<ToolContentPart> = parts
                .into_iter()
                .map(|p| {
                    if tool_part_matches(&p, target) {
                        ToolContentPart::Text {
                            text: placeholder.to_string(),
                        }
                    } else {
                        p
                    }
                })
                .collect();
            // Collapse single text part back to Text.
            if stripped.len() == 1 {
                if let ToolContentPart::Text { text } = &stripped[0] {
                    return ToolResultContent::Text(text.clone());
                }
            }
            ToolResultContent::Parts(stripped)
        }
        other => other,
    }
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
}
