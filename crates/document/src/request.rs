// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One request to the OCR model: page images and an instruction in, the text
//! and what it cost out, with the server's pushback waited out.

use std::time::{Duration, Instant};

use base64::Engine as _;
use futures::StreamExt;
use sven_model::{CompletionRequest, ContentPart, Message, ResponseEvent, SamplingParams};

use crate::backend::RenderedPage;
use crate::limiter::is_pushback;
use crate::page::PageContext;

/// Server pushbacks one request waits out before they count as failures.
const MAX_PUSHBACKS: u32 = 8;

pub(crate) struct Answer {
    pub text: String,
    pub cutoff: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// `images` and `prompt` asked of the endpoint the limiter gives, again
/// after each pushback (429, 503, a full queue) without counting it as a
/// failure. Answers how long the requests took, pushbacks included.
pub(crate) async fn ask_patiently(
    ctx: &PageContext,
    images: &[&RenderedPage],
    prompt: &str,
    max_tokens: u32,
) -> (Result<Answer, String>, u64) {
    let mut pushbacks = 0;
    let mut elapsed_ms = 0;
    loop {
        let slot = ctx.limiter.acquire().await;
        let started = Instant::now();
        let outcome = ask(ctx, slot.endpoint(), images, prompt, max_tokens).await;
        elapsed_ms += started.elapsed().as_millis() as u64;
        match outcome {
            Ok(answer) => {
                slot.succeeded();
                return (Ok(answer), elapsed_ms);
            }
            Err(e) if is_pushback(&e) && pushbacks < MAX_PUSHBACKS => {
                slot.overloaded();
                drop(slot);
                pushbacks += 1;
                tokio::time::sleep(Duration::from_millis(500 << pushbacks.min(5))).await;
            }
            Err(e) => return (Err(e), elapsed_ms),
        }
    }
}

async fn ask(
    ctx: &PageContext,
    endpoint: usize,
    images: &[&RenderedPage],
    prompt: &str,
    max_tokens: u32,
) -> Result<Answer, String> {
    let mut parts: Vec<ContentPart> = images
        .iter()
        .map(|page| {
            ContentPart::image(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&page.png)
            ))
        })
        .collect();
    parts.push(ContentPart::text(prompt.to_string()));
    let request = CompletionRequest {
        messages: vec![Message::user_with_parts(parts)],
        stream: true,
        max_output_tokens_override: Some(max_tokens),
        sampling: SamplingParams {
            temperature: Some(0.0),
            ..Default::default()
        },
        ..Default::default()
    };
    let exchange = async {
        let mut stream = ctx.providers[endpoint]
            .complete(request)
            .await
            .map_err(|e| format!("model request failed: {e:#}"))?;
        let mut answer = Answer {
            text: String::new(),
            cutoff: false,
            input_tokens: 0,
            output_tokens: 0,
        };
        while let Some(event) = stream.next().await {
            match event.map_err(|e| format!("model stream failed: {e:#}"))? {
                ResponseEvent::TextDelta(t) => answer.text.push_str(&t),
                ResponseEvent::MaxTokens => answer.cutoff = true,
                ResponseEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    answer.input_tokens += u64::from(input_tokens);
                    answer.output_tokens += u64::from(output_tokens);
                }
                ResponseEvent::Error(e) => return Err(format!("model error: {e}")),
                _ => {}
            }
        }
        Ok(answer)
    };
    tokio::time::timeout(ctx.options.request_timeout, exchange)
        .await
        .map_err(|_| "model request timed out".to_string())?
}
