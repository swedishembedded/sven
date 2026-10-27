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
mod checkpoint;
mod columns;
mod convert;
mod error;
mod layout;
mod limiter;
mod locate;
mod manifest;
mod names;
mod page;
mod pages;
mod quality;
mod query;
mod score;
mod validate;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use backend::{HayroBackend, PdfBackend, PdfSource, RenderedPage};
pub use convert::{
    ConvertOptions, Converter, LayoutMode, OcrMode, Progress, Report, DEFAULT_LAYOUT_PROMPT,
    DEFAULT_PROMPT,
};
pub use error::DocumentError;
pub use layout::{
    column_count, merge_runs, reading_order, DeepseekGrounding, LayoutParser, Region,
};
pub use locate::{find as find_text, outline, outline_matches, OutlineEntry, PageHit};
pub use manifest::{manifest_path, LayoutInfo, Manifest, Method, PageRecord, Status};
pub use names::{match_model, Candidate, MatchError};
pub use pages::parse_selection;
pub use quality::{assess as assess_native_text, TextQuality};
pub use query::{normalize as normalize_whitespace, Hit, Query, Syntax};
pub use score::character_error_rate;
pub use validate::Problem;
