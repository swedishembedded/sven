// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Where a generated step came from.
//!
//! A version 2 workflow lowers to many ordinary steps per authored task. The
//! source map names, for every generated step, the task, the role it plays
//! (work turn, follow-up, check, repair, success check) and the gate and
//! attempt it belongs to, so a trace line, a failure message and a graph can
//! be read in the author's terms. The map is part of the compiled plan and so
//! of its digest.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The authored origin of one generated step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// The authored task the step belongs to; `None` for the workflow's
    /// own success checks and completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// What the step does for its task: `when`, `work`, `follow_up`,
    /// `check`, `repair`, `failure`, `success`, `complete`.
    pub role: String,
    /// The gate or check the step evaluates or reports on, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// The attempt of the task's verification the step belongs to: 0 is
    /// the first verification, 1 the one after the first repair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
}

/// What lowering a version 2 workflow adds to the v1 spec it produces.
///
/// Never read from a document: a v1 document cannot carry it (the field is
/// skipped by serde), and only the v2 lowering sets it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lowering {
    /// The tool profile the workflow declared (`none` or `coding`).
    pub profile: String,
    /// The `execution.scratchpad` template (over inputs), when declared.
    pub scratchpad: Option<String>,
    /// Generated step id to its authored origin.
    pub source_map: BTreeMap<String, SourceRef>,
}
