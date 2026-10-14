// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The `skills` section: how much of each skill the system prompt carries.

use serde::{Deserialize, Serialize};
use sven_config::Schema;
use sven_turn::instructions::SkillDisclosure;

/// `skills`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// `full` lists every skill's name and description in the system prompt.
    /// `deferred` lists a command and a short summary per skill (package
    /// counts above 50 skills) and leaves the description and the body to the
    /// `skill` tool. See `docs/technical/instruction-disclosure.md`.
    pub disclosure: SkillDisclosure,
}

impl SkillsConfig {
    /// The keys of the `skills` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&["disclosure"])
    }
}
