// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred disclosure
// of tools and skills for agents then you can procure our services by sending
// an email to info@swedishembedded.com.
//! The `full | deferred` choice of `--tool-disclosure` and the `--*-disclosure` flags of the instruction catalogs.

use clap::{Args, ValueEnum};

/// How much of each tool definition or skill description a request carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DisclosureArg {
    /// Every tool's definition, or every skill's description, in every request.
    Full,
    /// Core and loaded tools in full, or a short summary per skill; the rest
    /// by summary until the model loads it.
    Deferred,
}

impl From<DisclosureArg> for sven_bootstrap::Disclosure {
    fn from(arg: DisclosureArg) -> Self {
        match arg {
            DisclosureArg::Full => Self::Full,
            DisclosureArg::Deferred => Self::Deferred,
        }
    }
}

impl From<DisclosureArg> for sven_bootstrap::ListingDisclosure {
    fn from(arg: DisclosureArg) -> Self {
        match arg {
            DisclosureArg::Full => Self::Full,
            DisclosureArg::Deferred => Self::Deferred,
        }
    }
}

/// The flags that set how much of each tool definition and catalog entry a
/// request carries, each overriding the configuration file for one process.
#[derive(Args, Debug)]
pub struct DisclosureFlags {
    /// How much of each tool's definition every request carries, overriding
    /// `tools.disclosure.mode` of the configuration file for this process.
    ///
    /// `full` sends every tool's definition. `deferred` sends the core tools'
    /// and the ones the model has loaded, and describes the rest in a line each:
    /// the model loads one through the `tools` tool before calling it. A `task`
    /// sub-agent runs as its own process and reads the configuration file.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        overrides_with = "tool_disclosure"
    )]
    pub tool_disclosure: Option<DisclosureArg>,

    /// How much of each skill the system prompt carries, overriding
    /// `skills.disclosure` of the configuration file for this process.
    ///
    /// `full` lists every skill's name and description. `deferred` lists a
    /// command and a short summary per skill; the description and the body
    /// load through the `skill` tool, which also searches the skills.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        overrides_with = "skill_disclosure"
    )]
    pub skill_disclosure: Option<DisclosureArg>,

    /// How much of each subagent the system prompt carries, overriding
    /// `subagents.disclosure` of the configuration file for this process.
    ///
    /// `deferred` lists a name and a short summary per subagent; `task` with
    /// `action: "agents"` returns the whole descriptions.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        overrides_with = "subagent_disclosure"
    )]
    pub subagent_disclosure: Option<DisclosureArg>,

    /// How much of each knowledge document the system prompt carries,
    /// overriding `knowledge.disclosure` of the configuration file for this
    /// process.
    ///
    /// `deferred` lists a subsystem and the files it covers per document;
    /// `search_knowledge` and `list_knowledge` return the rest.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        overrides_with = "knowledge_disclosure"
    )]
    pub knowledge_disclosure: Option<DisclosureArg>,
}

impl DisclosureFlags {
    /// Lays the flags that were given over `config`.
    pub fn apply(&self, config: &mut sven_bootstrap::Config) {
        if let Some(mode) = self.tool_disclosure {
            config.tools.disclosure.mode = mode.into();
        }
        if let Some(mode) = self.skill_disclosure {
            config.skills.disclosure = mode.into();
        }
        if let Some(mode) = self.subagent_disclosure {
            config.subagents.disclosure = mode.into();
        }
        if let Some(mode) = self.knowledge_disclosure {
            config.knowledge.disclosure = mode.into();
        }
    }
}
