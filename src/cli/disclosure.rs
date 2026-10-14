// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred disclosure
// of tools and skills for agents then you can procure our services by sending
// an email to info@swedishembedded.com.
//! The `full | deferred` choice of `--tool-disclosure` and `--skill-disclosure`.

use clap::ValueEnum;

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

impl From<DisclosureArg> for sven_bootstrap::SkillDisclosure {
    fn from(arg: DisclosureArg) -> Self {
        match arg {
            DisclosureArg::Full => Self::Full,
            DisclosureArg::Deferred => Self::Deferred,
        }
    }
}
