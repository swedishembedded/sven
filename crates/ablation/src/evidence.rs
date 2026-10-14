// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What a model was sent, read from the request log of the scripted
//! provider, and how much of the labelled evidence it contained.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::task::{Evidence, EvidenceRole};

/// One message of a logged request.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LoggedMessage {
    pub role: String,
    /// `text`, `tool_call` or `tool_result`.
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    pub text: String,
}

/// One request the scripted provider received.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LoggedRequest {
    pub request: u32,
    #[serde(default)]
    pub pid: u32,
    pub tools: Vec<String>,
    pub messages: Vec<LoggedMessage>,
}

impl LoggedRequest {
    /// Whether `marker` occurs in any message.
    #[must_use]
    pub fn contains(&self, marker: &str) -> bool {
        self.messages.iter().any(|m| m.text.contains(marker))
    }
}

/// The requests of one segment, in the order they were made.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RequestLog {
    pub requests: Vec<LoggedRequest>,
}

impl RequestLog {
    /// Reads a JSON-lines request log.
    ///
    /// # Errors
    ///
    /// A missing file or a line that is not a logged request. A log that is
    /// unreadable is an error and never an empty log: an empty log would
    /// score as "no evidence reached the model".
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parses JSON lines.
    ///
    /// # Errors
    ///
    /// The first line that is not a logged request, with its number.
    pub fn parse(text: &str) -> Result<Self, String> {
        let requests = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(n, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", n + 1)))
            .collect::<Result<Vec<LoggedRequest>, _>>()?;
        Ok(Self { requests })
    }

    /// The last request of the process that started the segment: what the
    /// model read when it answered. Requests of sub-agents, which share the
    /// log, are not it.
    #[must_use]
    pub fn final_request(&self) -> Option<&LoggedRequest> {
        let root = self.requests.first()?.pid;
        self.requests.iter().rev().find(|r| r.pid == root)
    }

    /// Whether `marker` occurs in any request at all.
    #[must_use]
    pub fn ever_contains(&self, marker: &str) -> bool {
        self.requests.iter().any(|r| r.contains(marker))
    }
}

/// How much of the labelled evidence reached the model in a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceScore {
    pub critical_total: usize,
    pub critical_visible: usize,
    pub distractor_total: usize,
    pub distractor_visible: usize,
    /// `critical_visible / critical_total`; 1 when the task has none.
    pub critical_recall: f64,
    /// Visible critical items over all visible labelled items; absent when
    /// nothing labelled is visible.
    pub precision: Option<f64>,
    /// Markers of the critical evidence that did not reach the model.
    pub missing: Vec<String>,
}

/// Scores `evidence` against what `request` carried.
#[must_use]
pub fn score_evidence(evidence: &[Evidence], request: &LoggedRequest) -> EvidenceScore {
    let visible: BTreeSet<&str> = evidence
        .iter()
        .filter(|e| request.contains(&e.marker))
        .map(|e| e.marker.as_str())
        .collect();
    let count = |role: EvidenceRole, only_visible: bool| {
        evidence
            .iter()
            .filter(|e| e.role == role && (!only_visible || visible.contains(e.marker.as_str())))
            .count()
    };
    let critical_total = count(EvidenceRole::Critical, false);
    let critical_visible = count(EvidenceRole::Critical, true);
    let distractor_visible = count(EvidenceRole::Distractor, true);
    let shown = critical_visible + distractor_visible;
    EvidenceScore {
        critical_total,
        critical_visible,
        distractor_total: count(EvidenceRole::Distractor, false),
        distractor_visible,
        critical_recall: if critical_total == 0 {
            1.0
        } else {
            critical_visible as f64 / critical_total as f64
        },
        precision: (shown > 0).then(|| critical_visible as f64 / shown as f64),
        missing: evidence
            .iter()
            .filter(|e| e.role == EvidenceRole::Critical && !visible.contains(e.marker.as_str()))
            .map(|e| e.marker.clone())
            .collect(),
    }
}
