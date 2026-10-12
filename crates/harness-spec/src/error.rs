// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Error vocabulary of the harness language and compiler.

use thiserror::Error;

/// Everything that can go wrong parsing, validating or compiling a harness.
#[derive(Debug, Error)]
pub enum HarnessError {
    /// The YAML itself is unreadable, or does not fit the typed spec.
    #[error("harness parse error: {0}")]
    Parse(String),
    /// The document parsed but is not a valid harness program. The message
    /// names the offending step, reference or contract.
    #[error("invalid harness: {0}")]
    Invalid(String),
    /// The document is not UTF-8.
    #[error("harness document is not UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}
