// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The testkit's error type: one variant per way a request can be unusable,
//! each carrying enough context (a path, a name) to act on.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::PathBuf;

/// Why a testkit operation could not produce its report.
#[derive(Debug, thiserror::Error)]
pub enum TestkitError {
    /// A file or directory could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A harness directory does not have the layout the testkit reads.
    #[error("{0}")]
    Layout(String),
    /// A case manifest or model fixture is malformed.
    #[error("{0}")]
    Manifest(String),
    /// A path in a manifest leaves the directory it is confined to.
    #[error("{0}")]
    Jail(String),
    /// A harness failed to parse, validate or compile, or its inputs do not
    /// fit it.
    #[error("{0}")]
    Harness(String),
    /// The run could not be carried out (not the plan's own failure).
    #[error("{0}")]
    Run(String),
    /// A live model was requested without the explicit opt-in.
    #[error("{0}")]
    LiveNotOptedIn(String),
}
