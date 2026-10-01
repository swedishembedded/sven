// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Attachment loading for the headless runner's `--attach` flag.
//!
//! Classification and loading go through `sven_tools_fs::load_attachment`, the
//! same function the `attach_file` tool uses, so the CLI flag and the tool can
//! never disagree about how a path becomes a content part.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;

use crate::output::write_stderr;

/// Build the initial user turn's content parts from the prompt plus `paths`.
pub(super) async fn build_attachment_parts(
    prompt: &str,
    paths: &[PathBuf],
    model: &Arc<dyn sven_model::ModelProvider>,
    asr: &sven_config::AsrConfig,
) -> anyhow::Result<Vec<sven_model::ContentPart>> {
    let opts = sven_tools_fs::AttachOptions {
        supports_images: model.supports_images(),
        supports_audio: model.supports_audio(),
        asr: asr.clone(),
        ..sven_tools_fs::AttachOptions::default()
    };
    let label = format!("{}/{}", model.name(), model.model_name());

    let mut parts = vec![sven_model::ContentPart::text(prompt)];
    for path in paths {
        let loaded = sven_tools_fs::load_attachment(path, &opts, &label)
            .await
            .with_context(|| format!("attaching {}", path.display()))?;
        write_stderr(&attach_notice(loaded.text()));
        parts.extend(loaded.into_content_parts());
    }

    // If every attachment resolved to text (e.g. all audio was transcribed),
    // merge into one text part.  `Message::user_with_parts` then collapses it
    // to a plain string message, so the turn is indistinguishable from an
    // ordinary prompt for every provider.
    if parts
        .iter()
        .all(|p| matches!(p, sven_model::ContentPart::Text { .. }))
    {
        let merged = parts
            .iter()
            .filter_map(|p| match p {
                sven_model::ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(vec![sven_model::ContentPart::text(merged)]);
    }

    Ok(parts)
}

/// What an attachment reports about itself on stderr.
///
/// The whole text, not its first line: an image's is one line already, but a
/// transcript's first line is only the `Transcript of <file> (Ns):` header.
/// Printing just that told the operator a clip had been transcribed while
/// hiding the one thing worth checking - what the model actually heard.
fn attach_notice(text: &str) -> String {
    format!("[sven:attach] {}", text.trim_end())
}

#[cfg(test)]
mod tests {
    use super::attach_notice;

    /// The operator has to be able to read back what was heard: a homophone
    /// ("Rust" for "rushed") is invisible in the header alone.
    #[test]
    fn a_transcript_notice_shows_the_transcribed_words() {
        let notice = attach_notice("Transcript of cmd.wav (2.0s):\n\nList the files here.");
        assert!(
            notice.starts_with("[sven:attach] Transcript of cmd.wav"),
            "{notice}"
        );
        assert!(notice.contains("List the files here."), "{notice}");
    }

    #[test]
    fn a_single_line_description_is_unchanged_apart_from_the_prefix() {
        let notice = attach_notice("Attached image: shot.png (1x1)");
        assert_eq!(notice, "[sven:attach] Attached image: shot.png (1x1)");
    }
}
