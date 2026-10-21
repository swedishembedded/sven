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
    /// An array-valued run variable (a workflow's `variables`), by name, read
    /// once when the iteration begins.
    Variable(String),
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

/// What a resumed run does with a `for_each` collection that may have
/// changed on disk since it was frozen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reconcile {
    /// The frozen list stands; nothing the workspace holds changes it.
    #[default]
    Frozen,
    /// At the first item boundary after a resume the collection is read
    /// again and merged by item id: unchanged completed items stay completed,
    /// a completed item whose content changed is pending again, new ids are
    /// appended, removed ids are dropped.
    Refresh,
}

impl Reconcile {
    /// `true` for the default, which a plan leaves out.
    #[must_use]
    pub fn is_frozen(&self) -> bool {
        *self == Self::Frozen
    }
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
    /// What a resume does with a changed collection.
    #[serde(default, skip_serializing_if = "Reconcile::is_frozen")]
    pub reconcile: Reconcile,
    /// Workspace files an item covers (templates over the item): their
    /// digest is recorded when the item completes, and a resume makes an item
    /// pending again whose files changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,
    /// Hand the host a durable checkpoint each time an item completes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checkpoint_each_item: bool,
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
    /// Names a run input that lowers the bound for this run (never raises it
    /// above `max_iterations`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations_input: Option<String>,
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
