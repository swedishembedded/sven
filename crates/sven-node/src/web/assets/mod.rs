// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Embeds the web terminal static assets (HTML, CSS, JS) into the binary.
//!
//! `rust-embed` compresses the assets at compile time with deflate.
//! Files are served from `/web/assets/*`.

use rust_embed::Embed;

/// All files under `src/web/assets/` are embedded at compile time.
#[derive(Embed)]
#[folder = "src/web/assets/"]
pub struct WebAssets;
