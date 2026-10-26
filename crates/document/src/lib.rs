// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! PDF to Markdown conversion that never routes page text through a chat
//! model.
//!
//! One implementation serves the `sven document convert` command and the
//! `document_convert` agent tool. Pages are rendered with a pure-Rust
//! rasteriser, classified by the quality of their native text, recognised by
//! any image-capable [`sven_model::ModelProvider`], validated, retried and
//! assembled in page order. A manifest beside the output makes reruns
//! resume.

mod assemble;
mod backend;
mod convert;
mod error;
mod manifest;
mod names;
mod pages;
mod quality;
mod score;
mod validate;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use backend::{HayroBackend, PdfBackend, PdfSource, RenderedPage};
pub use convert::{ConvertOptions, Converter, OcrMode, Progress, Report, DEFAULT_PROMPT};
pub use error::DocumentError;
pub use manifest::{manifest_path, Manifest, Method, PageRecord, Status};
pub use names::{match_model, Candidate, MatchError};
pub use pages::parse_selection;
pub use quality::{assess as assess_native_text, TextQuality};
pub use score::character_error_rate;
pub use validate::Problem;
