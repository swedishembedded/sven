// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping multimodal agent
// requests within what the serving model accepts, for its clients. If your
// team needs expertise in multimodal LLM serving then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The images of an outgoing request: what the model takes, and what to say
//! when the server refuses them anyway.

use sven_model::catalog::ImageLimits;
use sven_model::sanitize::{limit_images, strip_images_if_unsupported};
use sven_model::{Message, ModelProvider};

/// `messages` as the request carries them: images stripped for a model that
/// takes none, the oldest left out beyond the model's stated per-request
/// limits. Also returns those limits. The caller's own conversation is not
/// edited; this works on the copy that becomes the request.
pub(crate) fn fit(
    model: &dyn ModelProvider,
    messages: Vec<Message>,
) -> (Vec<Message>, ImageLimits) {
    let messages = strip_images_if_unsupported(messages, &model.input_modalities());
    let limits = model.image_limits();
    (limit_images(messages, limits), limits)
}

/// The error of a refused request, with context. When the server refused it
/// over an image limit sven was not told (`known` states none), say so:
/// nothing is retried and nothing is guessed from the message, because the
/// server states its limits on `/models` (`max_images_per_request`,
/// `max_image_pixels_per_request`).
pub(crate) fn note_refusal(error: anyhow::Error, known: ImageLimits) -> anyhow::Error {
    let text = format!("{error:#}").to_lowercase();
    if !known.is_known()
        && text.contains("image")
        && (text.contains("at most") || text.contains("too many"))
    {
        tracing::warn!(
            error = %error,
            "the server refused the request over an image limit it did not state on /models; \
             sven knows no limit to apply, so it sent every image"
        );
    }
    error.context("model completion failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_keeps_its_cause_under_the_turn_context() {
        let refused = anyhow::anyhow!("openai error 400: a request may carry at most 8 images");
        let e = note_refusal(refused, ImageLimits::default());
        let text = format!("{e:#}");
        assert!(text.starts_with("model completion failed"), "{text}");
        assert!(text.contains("at most 8 images"), "{text}");
    }
}
