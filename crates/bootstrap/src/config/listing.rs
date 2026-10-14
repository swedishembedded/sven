// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The `skills`, `subagents` and `knowledge` sections: how much of each entry
//! of that catalog the system prompt carries.

use serde::{Deserialize, Serialize};
use sven_config::Schema;
use sven_turn::instructions::ListingDisclosure;

/// `skills`, `subagents` and `knowledge`: the three sections have one shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListingConfig {
    /// `full` lists every entry's whole metadata in the system prompt.
    /// `deferred` lists a name and a short summary per entry (counts above 50
    /// entries) and leaves the details to the tool that serves the catalog.
    /// See `docs/technical/instruction-disclosure.md`.
    pub disclosure: ListingDisclosure,
}

impl ListingConfig {
    /// The keys of the section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&["disclosure"])
    }
}
