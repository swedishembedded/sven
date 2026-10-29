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
//! closure), the cache of served models ([`sven_model_catalog`], re-exported
//! here at [`catalog`]), and the `--model mock` test/dev providers
//! (`sven-model-mock`).
pub mod budget;
/// Re-exported from the standalone [`sven_model_catalog`] crate at this path
/// (`sven_model::catalog::*`), which call sites across the workspace use
/// (`sven_model::catalog::served_models()`, ...). The catalog itself
/// (`ModelCatalogEntry`, `served_models()`, `lookup()`, the on-disk
/// served-model cache) lives in `sven-model-catalog`, which has zero heavy
/// dependencies (no reqwest, no cloud SDKs) - like `sven-model` itself - so
/// re-exporting it here adds no heavy dependency.
pub use sven_model_catalog as catalog;
pub mod manifest;
mod provider;
pub mod registry;
pub mod repeat;
pub mod sanitize;
mod turns;
mod types;

pub use catalog::{InputModality, ModelCatalogEntry};
pub use manifest::{PromptManifest, PromptSections, SectionSpan, ToolDigest};
pub use provider::{
    AdapterRef, ModelChange, ModelProvider, ResponseStream, ServedModel, ServedModels,
};
pub use registry::{get_driver, list_drivers, DriverMeta};
pub use turns::{turns, Turn, TurnCall};
pub use types::*;
