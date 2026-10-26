// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The reasons a conversion cannot start or cannot be published.
//!
//! A page that fails does not appear here: it is recorded in the manifest and
//! counted in the report. These errors stop the whole run.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("not a readable PDF: {0}")]
    InvalidPdf(String),
    #[error("the PDF is encrypted and cannot be opened without a password")]
    Encrypted,
    #[error("the PDF has no pages")]
    Empty,
    #[error("invalid page selection '{0}': {1}")]
    PageSelection(String, String),
    #[error("{0}")]
    Model(String),
    #[error("pages need OCR but no OCR model was given")]
    NoModel,
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("conversion was cancelled")]
    Cancelled,
}
