// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Test fixtures: a small PDF writer and a scripted OCR model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use sven_model::catalog::InputModality;
use sven_model::{
    CompletionRequest, ContentPart, MessageContent, ModelProvider, ResponseEvent, ResponseStream,
};

/// What the scripted model sees of a request.
pub struct Seen {
    /// The gray level of the page image (its identity in these tests).
    pub gray: u8,
    pub width: u32,
    /// The instruction sent with the image.
    pub prompt: String,
}

pub struct Reply {
    pub text: String,
    pub cutoff: bool,
    pub delay: Duration,
    pub error: Option<String>,
}

impl Reply {
    pub fn text(t: impl Into<String>) -> Self {
        Self {
            text: t.into(),
            cutoff: false,
            delay: Duration::ZERO,
            error: None,
        }
    }
}

type Script = Box<dyn Fn(&Seen, usize) -> Reply + Send + Sync>;

/// An image-capable model whose answer is a function of the page image.
pub struct ScriptedOcr {
    pub calls: Arc<AtomicUsize>,
    script: Script,
    images: bool,
}

impl ScriptedOcr {
    /// `script(seen, call_index)`.
    pub fn new(script: impl Fn(&Seen, usize) -> Reply + Send + Sync + 'static) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            script: Box::new(script),
            images: true,
        }
    }

    pub fn text_only(mut self) -> Self {
        self.images = false;
        self
    }
}

#[async_trait]
impl ModelProvider for ScriptedOcr {
    fn name(&self) -> &str {
        "scripted"
    }
    fn model_name(&self) -> &str {
        "scripted-ocr"
    }
    fn input_modalities(&self) -> Vec<InputModality> {
        if self.images {
            vec![InputModality::Text, InputModality::Image]
        } else {
            vec![InputModality::Text]
        }
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let url = req
            .messages
            .iter()
            .find_map(|m| match &m.content {
                MessageContent::ContentParts(parts) => parts.iter().find_map(|p| match p {
                    ContentPart::Image { image_url, .. } => Some(image_url.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .expect("the request carries a page image");
        let b64 = url
            .strip_prefix("data:image/png;base64,")
            .expect("a PNG data URL");
        let png = base64::engine::general_purpose::STANDARD.decode(b64)?;
        let img = image::load_from_memory(&png)?.to_rgb8();
        let prompt = req
            .messages
            .iter()
            .find_map(|m| match &m.content {
                MessageContent::ContentParts(parts) => parts.iter().find_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_default();
        let seen = Seen {
            prompt,
            gray: img.get_pixel(img.width() / 2, img.height() / 2)[0],
            width: img.width(),
        };
        let reply = (self.script)(&seen, call);
        tokio::time::sleep(reply.delay).await;
        if let Some(e) = reply.error {
            anyhow::bail!(e);
        }
        let mut events = vec![ResponseEvent::TextDelta(reply.text)];
        if reply.cutoff {
            events.push(ResponseEvent::MaxTokens);
        }
        events.push(ResponseEvent::Done);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

pub use sven_document::testing::{build_pdf, FixturePage, PROSE};
