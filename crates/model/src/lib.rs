// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel-tier vocabulary every LLM-facing crate in the workspace
//! shares: the [`ModelProvider`] trait, the request/response types
//! (`CompletionRequest`, `ResponseEvent`, `Message`, ...), the pure
//! prompt-size budget gate (`budget`), image-support sanitisation
//! (`sanitize`) and the static driver registry (`registry`).
//!
//! Deliberately **not** here: the concrete driver implementations, the
//! `from_config`/`from_config_probed` factory that constructs them and the
//! model-string resolution that feeds it (`sven-model-drivers`, which owns
//! `reqwest`, the user's configuration and every provider's own dependency
//! closure), the static catalog data ([`sven_model_catalog`], re-exported
//! here at [`catalog`]), and the `--model mock` test/dev providers
//! (`sven-model-mock`).
pub mod budget;
/// Re-exported from the standalone [`sven_model_catalog`] crate at this path
/// (`sven_model::catalog::*`), which call sites across the workspace use
/// (`sven_model::catalog::static_catalog()`, ...). The catalog itself
/// (`ModelCatalogEntry`, `static_catalog()`, `lookup()`, the on-disk
/// live-cache overlay) lives in `sven-model-catalog`, which has zero heavy
/// dependencies (no reqwest, no cloud SDKs) — like `sven-model` itself — so
/// re-exporting it here adds no heavy dependency.
pub use sven_model_catalog as catalog;
mod provider;
pub mod registry;
pub mod sanitize;
mod types;

pub use catalog::{InputModality, ModelCatalogEntry};
pub use provider::{ModelProvider, ResponseStream};
pub use registry::{get_driver, list_drivers, DriverMeta};
pub use types::*;
