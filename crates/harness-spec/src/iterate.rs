// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `iterate` step's specification: what repeats and how it is bounded.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde::{Deserialize, Serialize};

/// Where a `for_each` reads its collection from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IterateSource {
    /// An array in a JSON file in the workspace, read once when the
    /// iteration begins.
    Json(JsonSource),
    /// An array-valued run input, by name.
    Input(String),
}

/// A value inside a JSON file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonSource {
    /// Workspace-relative path.
    pub file: String,
    /// RFC 6901 pointer to the array (`""` is the whole document).
    pub pointer: String,
}

/// `for_each` of an `iterate` step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForEachSpec {
    /// The collection.
    pub source: IterateSource,
    /// The field of each item that names it; omitted, the items are
    /// scalars and name themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The most items taken; a larger collection fails the run.
    #[serde(default = "default_max_items")]
    pub max_items: u64,
}

fn default_max_items() -> u64 {
    crate::spec::DEFAULT_MAX_ITEMS
}

/// `until` of an `iterate` step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UntilSpec {
    /// The most times the body is entered; one more entry goes to
    /// `on_exhausted`.
    pub max_iterations: u64,
}

/// `iterate`: the one repetition primitive.
///
/// Each time the walk reaches the step it decides what runs next. With
/// `for_each` the collection is read and frozen on the first visit; each
/// later visit marks the item whose body just finished as completed and starts
/// the next, and the visit after the last continues at `then`. With `until`
/// each visit enters the body again, counting, until `max_iterations` entries
/// have happened. The body is ordinary steps whose last edge returns here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IterateStepSpec {
    /// Run the body once per item of a frozen collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<ForEachSpec>,
    /// Run the body until something else ends the loop, at most this often.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<UntilSpec>,
    /// The first step of the body.
    pub body: String,
    /// Where to continue when the loop is finished.
    pub then: String,
    /// Where to continue when an `until` loop has used its entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<String>,
    /// Where to store the summary `{total, completed, pending}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
}
