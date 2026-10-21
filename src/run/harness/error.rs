// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The error type of the harness commands: one variant per way a request can
//! be unusable, each carrying enough context (a path, a name) to act on.
//!
//! Swedish Embedded AB implements reviewable, declarative agent workflows for
//! its clients. If your team needs expertise in explaining what a long-running
//! agent job does before it runs then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::PathBuf;

/// Why a harness command could not produce its report.
#[derive(Debug, thiserror::Error)]
pub(super) enum HarnessError {
    /// A file or directory could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A harness directory does not hold `harness.yaml`.
    #[error("{0}")]
    Layout(String),
    /// A harness failed to parse, validate or compile, or its inputs do not
    /// fit it, or a file given for it is not what it should be.
    #[error("{0}")]
    Harness(String),
    /// The run could not be carried out (not the plan's own failure).
    #[error("{0}")]
    Run(String),
}
