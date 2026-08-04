// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Header-only quick read, adapted from `sven-input`'s `chat_document.rs`
//! `ChatDocumentHeader` trick (find the marker before the large array, parse
//! only what precedes it) — for JSON instead of YAML.
//!
//! [`crate::model::Trajectory`] deliberately declares `steps` as its last
//! Rust struct field, and `serde_json` preserves struct declaration order
//! when serializing, so on any file this crate wrote, `"steps"` is the last
//! top-level key. [`read_trajectory_header_fast`] exploits that: it scans
//! for the byte offset of the top-level `"steps"` key (tracking brace/bracket
//! depth and string escaping so it can't be fooled by a `"steps"` string
//! appearing inside a nested `extra` value), truncates the text there, closes
//! it into a small valid JSON object, and deserializes only that into
//! [`TrajectoryHeader`] — never touching the (possibly huge) steps array.
//!
//! [`read_trajectory_header`] is the defensive public entry point: it tries
//! the fast path first and falls back to a full [`crate::model::Trajectory`]
//! parse (reduced to a header) on *any* failure — a foreign file where
//! `steps` isn't last, a truncated file, whatever.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::model::{AgentProfile, FinalMetrics, Trajectory};

/// Errors from the header-only reader.
#[derive(Debug, Error)]
pub enum HeaderReadError {
    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    /// The fast-path heuristic could not locate a usable split point (e.g.
    /// `steps` isn't the last top-level key, or the file is too short/odd
    /// to contain one).
    #[error("could not locate a top-level \"steps\" key to split on")]
    NoSplitPoint,
}

/// Everything in a [`crate::model::Trajectory`] except `steps` and
/// `subagent_trajectories` — cheap to obtain even from a huge trajectory
/// file, for listing many trajectories without deserializing their step
/// arrays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryHeader {
    /// See [`Trajectory::schema_version`].
    pub schema_version: String,
    /// See [`Trajectory::session_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// See [`Trajectory::trajectory_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    /// See [`Trajectory::agent`].
    pub agent: AgentProfile,
    /// See [`Trajectory::notes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// See [`Trajectory::final_metrics`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<FinalMetrics>,
    /// See [`Trajectory::extra`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl From<&Trajectory> for TrajectoryHeader {
    fn from(t: &Trajectory) -> Self {
        Self {
            schema_version: t.schema_version.clone(),
            session_id: t.session_id.clone(),
            trajectory_id: t.trajectory_id.clone(),
            agent: t.agent.clone(),
            notes: t.notes.clone(),
            final_metrics: t.final_metrics.clone(),
            extra: t.extra.clone(),
        }
    }
}

/// Find the byte offset of the comma that immediately precedes a top-level
/// (depth-1) `"steps"` key, by scanning byte-by-byte and tracking
/// object/array nesting depth and string-literal escaping.
///
/// Returns `None` if `"steps"` never appears at depth 1, or if it does but
/// has no preceding top-level comma (i.e. it would be the very first key,
/// which can't happen for a valid `Trajectory` since `schema_version` is
/// always emitted first — treated as "no usable split point").
fn find_steps_split_point(json: &str) -> Option<usize> {
    let bytes = json.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut last_top_level_comma: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                if depth == 1 && json[i..].starts_with("\"steps\"") {
                    return last_top_level_comma;
                }
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b',' if depth == 1 => last_top_level_comma = Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The strict fast path: locate the top-level `"steps"` key, truncate before
/// it, close the object, and parse only that into a [`TrajectoryHeader`].
/// Returns [`HeaderReadError`] (never panics) if the heuristic can't find a
/// usable split point or the truncated text doesn't parse — callers that
/// want automatic fallback should use [`read_trajectory_header`] instead.
pub fn read_trajectory_header_fast(path: &Path) -> Result<TrajectoryHeader, HeaderReadError> {
    let content = fs::read_to_string(path)?;
    let split_at = find_steps_split_point(&content).ok_or(HeaderReadError::NoSplitPoint)?;
    let mut header_json = String::with_capacity(split_at + 2);
    header_json.push_str(&content[..split_at]);
    header_json.push('}');
    let header: TrajectoryHeader = serde_json::from_str(&header_json)?;
    Ok(header)
}

/// Defensive public entry point: try [`read_trajectory_header_fast`] first;
/// on any failure, fall back to a full [`Trajectory`] parse and reduce it to
/// a [`TrajectoryHeader`]. Only errors if *both* the fast path and the full
/// parse fail (e.g. a genuinely truncated/corrupt file).
pub fn read_trajectory_header(path: &Path) -> Result<TrajectoryHeader, HeaderReadError> {
    if let Ok(header) = read_trajectory_header_fast(path) {
        return Ok(header);
    }
    let content = fs::read_to_string(path)?;
    let full: Trajectory = serde_json::from_str(&content)?;
    Ok(TrajectoryHeader::from(&full))
}
