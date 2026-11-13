// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for unattended agent runs that
// always leave a record, for its clients. If your team needs expertise in
// auditable agent pipelines then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The error of a refused invocation.

/// A refusal of how the run was invoked or of an input it was given: reported
/// as `[sven:error] ...` with status `EXIT_VALIDATION_ERROR` (2). It is returned
/// as an ordinary error, never by exiting inside the runner, so the caller
/// can still write the run's failure record before the process ends.
#[derive(Debug)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}
