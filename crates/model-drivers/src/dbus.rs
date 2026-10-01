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
//! # Scope: one-shot `Run` only (deliberate)
//!
//! brain's interface also exposes a streaming `Subscribe` method, which is
//! **not** implemented here and is not an oversight.  `Subscribe` hands back a
//! `SOCK_SEQPACKET` socket over which the server pushes framed events with
//! `SCM_RIGHTS` ancillary data; consuming it means writing a framing protocol
//! reader, an ancillary-fd receiver, and an event demultiplexer — a subsystem
//! considerably larger than this whole transport.  The payoff would be
//! incremental token display, but the model this transport exists for
//! (`brain/omni`) emits only coarse progress rather than per-token deltas, so
//! there is nothing to stream.  [`DbusProvider`]'s
//! [`sven_model::ModelProvider::complete`] therefore performs one `Run` call and
//! emits the full response as a single `TextDelta`.
//!
//! Adding streaming later means: a `subscribe()` method on the `Manager`
//! interface behind [`proxy::ManagerProxy`] returning the socket fd, a
//! reader task that parses frames off that socket
//! and forwards each as a `ResponseEvent`, and swapping
//! [`DbusProvider`]'s one-shot `stream::iter(...)` for that task's receiver — the rest of the
//! transport (params flattening, blob encoding, reply decoding) is unchanged.
//!
//! The same connection carries brain's generic capability surface:
//! [`ActionClient`] runs any action a served model advertises (transcription,
//! image generation, ...), which is how the `attach_file` tool transcribes
//! audio for a model that cannot hear.
//!
//! Compiled only on Unix with the `dbus` feature. Without it the provider name
//! is still recognised, and refused with the reason instead of falling through
//! to the HTTP catch-all.

use sven_config::ModelConfig;
use sven_model::ModelProvider;

#[cfg(all(unix, feature = "dbus"))]
pub mod action;
#[cfg(all(unix, feature = "dbus"))]
pub mod blob;
#[cfg(all(unix, feature = "dbus"))]
mod provider;
#[cfg(all(unix, feature = "dbus"))]
pub mod proxy;

#[cfg(all(unix, feature = "dbus"))]
pub use action::{ActionClient, ActionInput, ActionOutcome};
#[cfg(all(unix, feature = "dbus"))]
pub use provider::{extract_blob_urls, flatten_messages, DbusOptions, DbusProvider};
#[cfg(all(unix, feature = "dbus"))]
pub use proxy::BusKind;

/// The D-Bus provider for `cfg`. It connects lazily, on the first request.
#[cfg(all(unix, feature = "dbus"))]
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
#[cfg(not(all(unix, feature = "dbus")))]
pub(crate) fn build(
    _cfg: &ModelConfig,
    _max_tokens: Option<u32>,
    _temperature: Option<f32>,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    anyhow::bail!(
        "the 'dbus' model provider is not available in this build \
         (requires a Unix target and the `dbus` feature)"
    )
}
