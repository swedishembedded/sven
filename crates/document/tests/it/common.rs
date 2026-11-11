// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Test fixtures: a small PDF writer and a scripted OCR model that sees every
//! page image of a request.

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
    /// The gray level of the (first) page image (its identity in these tests).
    pub gray: u8,
    /// The gray level of every image of the request, in order.
    pub grays: Vec<u8>,
    pub width: u32,
    /// The instruction sent with the image.
    pub prompt: String,
}

pub struct Reply {
    pub text: String,
    pub cutoff: bool,
    pub delay: Duration,
    pub error: Option<String>,
    /// Held until this many requests are in flight together (at most five
    /// seconds), so a test of concurrency does not depend on how fast the
    /// machine renders pages.
    pub hold: Option<Arc<tokio::sync::Barrier>>,
}

impl Reply {
    pub fn text(t: impl Into<String>) -> Self {
        Self {
            text: t.into(),
            cutoff: false,
            delay: Duration::ZERO,
            error: None,
            hold: None,
        }
    }
}

type Script = Box<dyn Fn(&Seen, usize) -> Reply + Send + Sync>;

/// An image-capable model whose answer is a function of the page image.
pub struct ScriptedOcr {
    pub calls: Arc<AtomicUsize>,
    script: Script,
    images: bool,
    max_output_tokens: Option<u32>,
}

impl ScriptedOcr {
    /// `script(seen, call_index)`.
    pub fn new(script: impl Fn(&Seen, usize) -> Reply + Send + Sync + 'static) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            script: Box::new(script),
            images: true,
            max_output_tokens: None,
        }
    }

    /// The output limit the model's listing states.
    pub fn listing_max_output(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
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
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let urls: Vec<String> = req
            .messages
            .iter()
            .flat_map(|m| match &m.content {
                MessageContent::ContentParts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Image { image_url, .. } => Some(image_url.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        assert!(!urls.is_empty(), "the request carries a page image");
        let mut images = Vec::new();
        for url in &urls {
            let b64 = url
                .strip_prefix("data:image/png;base64,")
                .expect("a PNG data URL");
            let png = base64::engine::general_purpose::STANDARD.decode(b64)?;
            images.push(image::load_from_memory(&png)?.to_rgb8());
        }
        let grays = images
            .iter()
            .map(|img| img.get_pixel(img.width() / 2, img.height() / 2)[0])
            .collect::<Vec<_>>();
        let img = &images[0];
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
            gray: grays[0],
            grays,
            width: img.width(),
        };
        let reply = (self.script)(&seen, call);
        if let Some(together) = &reply.hold {
            let _ = tokio::time::timeout(Duration::from_secs(5), together.wait()).await;
        }
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
