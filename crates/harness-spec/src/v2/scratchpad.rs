// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `execution.scratchpad`: where the run's private directory lives.
//!
//! The directory exists before the first step runs, so its location can only
//! depend on what the host handed the run: its inputs. The template is kept in
//! the plan; the runtime renders it when the run starts.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in keeping the files of an
//! automated run out of the repository it works on then you can procure our
//! services by sending an email to info@swedishembedded.com.

use super::context::Context;
use super::rewrite::rewrite_text;
use crate::error::HarnessError;
use crate::expr::Template;

/// The scratchpad template of the document, checked to read inputs only.
///
/// # Errors
///
/// An empty template, or one that reads anything but a declared input.
pub(super) fn lower(cx: &Context<'_>) -> Result<Option<String>, HarnessError> {
    let Some(text) = cx.w.execution.scratchpad.as_deref() else {
        return Ok(None);
    };
    let place = "execution.scratchpad";
    let template =
        Template::parse(text).map_err(|e| HarnessError::Invalid(format!("{place}: {e}")))?;
    if text.trim().is_empty() {
        return Err(HarnessError::Invalid(format!(
            "{place}: names no directory; write an absolute path, which may read inputs"
        )));
    }
    if let Some(path) = template.paths().iter().find(|p| p.root() != "inputs") {
        return Err(HarnessError::Invalid(format!(
            "{place}: `{path}` is not available here; the scratchpad is made before the run \
             starts, so its path may read `inputs.<name>` only"
        )));
    }
    rewrite_text(text, &cx.scope(place.to_string(), &[], Default::default())).map(Some)
}
