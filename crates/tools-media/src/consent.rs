// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving people control over
// where their data goes for its clients. If your team needs expertise in
// consent controls for AI agents then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Asking the person before a model that may keep data runs: a run refused
//! because they have not allowed sharing data with the model is put to them,
//! where someone can answer (an interactive terminal), and run again once
//! they allow it. Where nobody can, nothing asks: the refusal's explanation
//! is the tool's answer, and a run with nobody to ask is never held up.

use async_trait::async_trait;

/// What the person answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingAnswer {
    /// This call only.
    Once,
    /// From now on: the model is allowed under their settings.
    Always,
    Deny,
}

/// Puts the question to the person.
#[async_trait]
pub trait SharingConsent: Send + Sync {
    /// Asks whether `model`, which may keep what it is sent, may be used.
    async fn ask(&self, model: &str) -> SharingAnswer;
}

/// The question's choices, in the order shown.
pub const CHOICES: [&str; 3] = ["Allow once", "Always allow this model", "Deny"];

/// The question's words.
#[must_use]
pub fn prompt(model: &str) -> String {
    format!("{model} may keep what you send it. Its provider may keep the prompt and anything it is given. Allow it?")
}

/// The answer a person's chosen option names; anything else (another answer,
/// none) is a denial: silence is never consent.
#[must_use]
pub fn answer_of(chosen: &str) -> SharingAnswer {
    match chosen.trim() {
        c if c == CHOICES[0] => SharingAnswer::Once,
        c if c == CHOICES[1] => SharingAnswer::Always,
        _ => SharingAnswer::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_named_choice_allows() {
        assert_eq!(answer_of("Allow once"), SharingAnswer::Once);
        assert_eq!(answer_of("Always allow this model"), SharingAnswer::Always);
        for other in ["Deny", "Other: maybe", "(no selection)", ""] {
            assert_eq!(answer_of(other), SharingAnswer::Deny, "{other}");
        }
    }
}
