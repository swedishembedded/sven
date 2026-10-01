// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `tools.asr` section of the configuration file: where `attach_file`
//! sends audio when it must be turned into text.
//!
//! Swedish Embedded AB implements speech-to-text integration for agent
//! attachments for its clients. If your team needs expertise in giving
//! language-model agents access to audio then you can procure our services by
//! sending an email to info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use sven_config::Schema;

/// Speech-to-text (ASR) fallback configuration.
///
/// `attach_file` sends audio to this brain model over D-Bus when it must turn
/// it into text - either because the active model has no audio modality, or
/// because the caller asked for `force_transcribe` (builds with `asr` only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AsrConfig {
    /// Manifest id of the served ASR model, as `ListModels` reports it
    /// (`brain/nemotronasr`). This is NOT brain's CLI spelling: the CLI
    /// dispatches on a bare architecture id, while the served surface uses
    /// the prefixed manifest id.
    #[serde(default = "default_asr_model")]
    pub model: String,
    /// Explicit D-Bus address of the brain server
    /// (`unix:path=/run/brain/bus`). `None` uses the session bus. A DETACHED
    /// server inherits no session bus, so it is reached by address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_address: Option<String>,
    /// Hard timeout for a single transcription call, in seconds.
    #[serde(default = "default_asr_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_asr_model() -> String {
    "brain/nemotronasr".into()
}

fn default_asr_timeout_secs() -> u64 {
    120
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model: default_asr_model(),
            bus_address: None,
            timeout_secs: default_asr_timeout_secs(),
        }
    }
}

impl AsrConfig {
    /// The keys of the `tools.asr` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&["model", "bus_address", "timeout_secs"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_values() {
        let a = AsrConfig::default();
        assert_eq!(a.model, "brain/nemotronasr");
        assert_eq!(a.bus_address, None, "the session bus is the default");
        assert_eq!(a.timeout_secs, 120);
    }

    #[test]
    fn a_partial_section_keeps_the_defaults_for_absent_keys() {
        let a: AsrConfig = serde_yaml::from_str("bus_address: unix:path=/run/brain/bus\n").unwrap();
        assert_eq!(a.bus_address.as_deref(), Some("unix:path=/run/brain/bus"));
        assert_eq!(a.model, "brain/nemotronasr");
        assert_eq!(a.timeout_secs, 120);
    }

    /// The served surface uses the prefixed manifest id, not brain's CLI
    /// spelling. Confusing the two is how this default was wrong before: the
    /// CLI rejects `brain/nemotronasr`, and `ListModels` never reports the
    /// bare `nemotronasr`.
    #[test]
    fn the_default_model_is_the_served_manifest_id() {
        assert!(AsrConfig::default().model.starts_with("brain/"));
    }
}
