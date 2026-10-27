// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! D-Bus model transport for brain's `com.swedishembedded.Brain1` service.
//!
//! This driver talks to a locally running brain server over D-Bus instead of
//! HTTP.  Its reason to exist is multimodality with zero encode/decode
//! round-trips through a JSON body: images and audio travel as raw tensors on
//! sealed `memfd` file descriptors rather than as base64 inside the request.
//!
//! ```yaml
//! providers:
//!   brain_dbus:
//!     name: dbus
//!     input_modalities: [text, image, audio]
//!     driver_options:
//!       bus: session
//!       service: com.swedishembedded.Brain1
//!       object_path: /com/swedishembedded/Brain1
//!       action: generate
//!       max_image_dim: 1024
//!     models:
//!       brain/omni:
//!         max_tokens: 32768
//! ```
//!
//! # Streaming over `Subscribe`
//!
//! [`DbusProvider`]'s [`sven_model::ModelProvider::complete`] calls brain's
//! `Subscribe`, which returns at once with a job id and a `SOCK_SEQPACKET`
//! socket; brain pushes JSON frames over it (content deltas, reasoning and
//! tool-call events, `SCM_RIGHTS` blobs, then `done` or `error`). The response
//! stream therefore starts when the job is admitted and text arrives as it is
//! generated, so a queued or long generation under parallel load never trips
//! the first-byte timeout. Dropping the stream before `done` cancels the job
//! (`stream::CancelOnDrop`). Framing and translation live in `stream`.
//!
//! # Tool calling
//!
//! A request's tools travel as brain's `tools` parameter in the OpenAI
//! function-calling form (and `tool_choice` when it is `none`). The reply's
//! `reasoning_content` and `tool_calls` come back as thinking and tool-call
//! events after the text, the order a streamed reply would emit them in
//! (`reply::reply_events`).
//!
//! The same connection carries brain's generic capability surface:
//! [`ActionClient`] runs any action a served model advertises (transcription,
//! image generation, ...), which is how the `attach_file` tool transcribes
//! audio for a model that cannot hear.
//!
//! Compiled only on Linux with the `dbus` feature. Without it the provider name
//! is still recognised, and refused with the reason instead of falling through
//! to the HTTP catch-all.

use crate::config::ModelConfig;
use sven_model::ModelProvider;

#[cfg(all(target_os = "linux", feature = "dbus"))]
pub mod action;
#[cfg(all(target_os = "linux", feature = "dbus"))]
pub mod blob;
#[cfg(all(target_os = "linux", feature = "dbus"))]
mod provider;
#[cfg(all(target_os = "linux", feature = "dbus"))]
pub mod proxy;
#[cfg(all(target_os = "linux", feature = "dbus"))]
mod reply;
#[cfg(all(target_os = "linux", feature = "dbus"))]
mod stream;

#[cfg(all(target_os = "linux", feature = "dbus"))]
pub use action::{ActionClient, ActionInput, ActionOutcome};
#[cfg(all(target_os = "linux", feature = "dbus"))]
pub use provider::{extract_blob_urls, flatten_messages, DbusOptions, DbusProvider};
#[cfg(all(target_os = "linux", feature = "dbus"))]
pub use proxy::BusKind;

/// The D-Bus provider for `cfg`. It connects lazily, on the first request.
#[cfg(all(target_os = "linux", feature = "dbus"))]
pub(crate) fn build(
    cfg: &ModelConfig,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    Ok(Box::new(DbusProvider::new(
        cfg.name.clone(),
        DbusOptions::from_driver_options(&cfg.driver_options),
        max_tokens,
        temperature,
    )))
}

/// Without the transport the provider name is still recognised, and refused
/// with the reason instead of falling through to the HTTP catch-all.
#[cfg(not(all(target_os = "linux", feature = "dbus")))]
pub(crate) fn build(
    _cfg: &ModelConfig,
    _max_tokens: Option<u32>,
    _temperature: Option<f32>,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    anyhow::bail!(
        "the 'dbus' model provider is not available in this build \
         (requires a Linux target and the `dbus` feature)"
    )
}
