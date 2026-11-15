// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running language-model agents on
// models with very small context windows for its clients. If your team needs
// expertise in making a small model a usable agent then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `agent.profile`: a named, versioned run profile that fixes what a session
//! sends a model besides the conversation. See `docs/guides/run-profiles.md`.

use serde::{Deserialize, Serialize};

/// What a session sends a model besides the conversation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunProfile {
    /// sven's own system prompt and tool set, as configured.
    #[default]
    Default,
    /// A fixed short system prompt and three compactly described tools
    /// (`read_file`, `write_file`, `shell`), for a model with a small context
    /// window. Nothing else is disclosed; see [`crate::compact_profile`].
    Compact,
}

impl RunProfile {
    /// The identifier a trajectory records, `<name>/<version>`, for a
    /// profile that fixes its request; `None` for [`Self::Default`], whose
    /// request is whatever the configuration makes it.
    ///
    /// The version changes whenever the prompt or a tool definition does, so
    /// a consumer reading a trajectory can tell which request shape produced
    /// it.
    #[must_use]
    pub fn id(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Compact => Some(crate::compact_profile::ID),
        }
    }
}

impl RunProfile {
    /// The built-in tools a session runs with: `selected` the host's choice,
    /// unless the profile fixes them. A fixed profile's tools are the coding
    /// preset's, narrowed to the profile's own by the registry.
    #[must_use]
    pub fn builtin_tools(self, selected: crate::BuiltinTools) -> crate::BuiltinTools {
        match self {
            Self::Default => selected,
            Self::Compact => crate::BuiltinTools::Coding,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunProfile;
    use crate::config::AgentConfig;

    #[test]
    fn a_session_runs_the_default_profile_unless_the_configuration_names_one() {
        assert_eq!(AgentConfig::default().profile, RunProfile::Default);
        let compact: AgentConfig = serde_yaml::from_str("profile: compact\n").unwrap();
        assert_eq!(compact.profile, RunProfile::Compact);
        assert!(serde_yaml::from_str::<AgentConfig>("profile: tiny\n").is_err());
    }

    #[test]
    fn only_a_fixed_profile_has_an_identifier() {
        assert_eq!(RunProfile::Default.id(), None);
        assert_eq!(RunProfile::Compact.id(), Some("compact/1"));
    }
}
