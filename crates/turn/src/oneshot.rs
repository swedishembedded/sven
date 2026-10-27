// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for hung and silently stalled model
// connections for its clients. If your team needs expertise in resilient
// agent runtimes then you can procure our services by sending an email to
// info@swedishembedded.com.
//! One-shot text completions that cannot hang.
//!
//! Title generation and context sub-queries ask a model one question without
//! tools and read the whole answer. They get the same stall protection as a
//! full turn - first byte, first real output, no progress, stream idle - from
//! the same [`StallWatch`], rather than an `await` that waits as long as the
//! server cares to stay quiet.

use sven_model::{CompletionRequest, ModelProvider, ResponseEvent};
use tokio::sync::mpsc;

use crate::stall::StallWatch;
use crate::stream_turn::TurnLimits;

/// Runs `req` against `model` and returns the text of the answer, bounded by
/// `limits` (see [`crate::StallLimits`] and [`TurnLimits::stream_idle`]).
///
/// Reasoning output is not part of the answer. The answer ends at the
/// stream's `Done` or its end: a `Usage` event is not an end, since some
/// drivers report input usage before the first token.
///
/// # Errors
///
/// Fails when the request fails, the server stalls past one of its limits, or
/// the stream reports an error. Nobody listens for the progress notices a
/// full turn would send, so they are dropped.
pub async fn complete_text(
    model: &dyn ModelProvider,
    req: CompletionRequest,
    limits: impl Into<TurnLimits>,
) -> anyhow::Result<String> {
    let limits = limits.into();
    let (notices, _unheard) = mpsc::channel(1);
    let mut watch = StallWatch::new(model, &limits.stall, limits.stream_idle);
    let mut stream = watch
        .connect(
            async {
                model
                    .complete(req)
                    .await
                    .map_err(|e| e.context("model completion failed"))
            },
            &notices,
        )
        .await?;

    let mut text = String::new();
    while let Some(event) = watch.next_event(&mut stream, &notices).await? {
        match event? {
            ResponseEvent::TextDelta(delta) => text.push_str(&delta),
            ResponseEvent::Done => break,
            ResponseEvent::Error(e) => anyhow::bail!("model stream error: {e}"),
            _ => {}
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::stream;
    use sven_model::ResponseStream;
    use sven_model_mock::{StallMode, StallingMockProvider};

    use super::*;
    use crate::StallLimits;

    /// Replays fixed events, as a driver that reports usage up front does.
    struct Scripted(Vec<ResponseEvent>);

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ModelProvider for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        fn model_name(&self) -> &str {
            "scripted"
        }
        async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
            let events: Vec<_> = self.0.clone().into_iter().map(Ok).collect();
            Ok(Box::pin(stream::iter(events)))
        }
    }

    fn quick_limits() -> TurnLimits {
        TurnLimits::default().with_stall_limits(StallLimits::from_secs(
            Some(30),
            Some(60),
            Some(90),
        ))
    }

    #[tokio::test]
    async fn usage_before_the_first_token_does_not_end_the_answer() {
        let model = Scripted(vec![
            ResponseEvent::Usage {
                input_tokens: 12,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            },
            ResponseEvent::ThinkingDelta("hmm".into()),
            ResponseEvent::TextDelta("Fix ".into()),
            ResponseEvent::TextDelta("login".into()),
            ResponseEvent::Done,
        ]);
        let text = complete_text(&model, CompletionRequest::default(), quick_limits())
            .await
            .unwrap();
        assert_eq!(text, "Fix login");
    }

    #[tokio::test]
    async fn a_stream_error_event_fails_the_completion() {
        let model = Scripted(vec![ResponseEvent::Error("overloaded".into())]);
        let err = complete_text(&model, CompletionRequest::default(), quick_limits())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("overloaded"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_answers_fails_at_the_first_byte_limit() {
        let model = StallingMockProvider::new(StallMode::NeverReturns);
        let err = complete_text(&model, CompletionRequest::default(), quick_limits())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no response headers"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_only_sends_keepalives_fails_at_the_first_token_limit() {
        let model = StallingMockProvider::new(StallMode::KeepaliveOnly {
            interval: Duration::from_secs(10),
        });
        let err = complete_text(&model, CompletionRequest::default(), quick_limits())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("only keepalives"), "{err}");
    }
}
