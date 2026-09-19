// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The three documents an arm's workspace can contain, and the properties they
//! must have for a result from that arm to mean anything.
//!
//! These are compiled in rather than read from disk so the tests below run
//! against exactly the bytes the sample writes into a workspace.

/// The structural specification. Complete as to shape, and deliberately
/// missing every value the task needs.
pub const SPEC: &str = include_str!("../spec/svf.md");

/// The knowledge under test.
pub const ERRATA: &str = include_str!("../spec/svf-errata.md");

/// A same-shaped note about a different format, for the control arm.
pub const DECOY: &str = include_str!("../spec/svf-errata-decoy.md");

/// Lowercased with all whitespace removed, so `C3 5A 1F 84` split across a
/// line break still matches `c35a1f84`. Deliberately over-sensitive: a false
/// positive here costs a minute, and a false negative costs the experiment.
#[must_use]
pub fn flatten(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{instances, svf};

    #[test]
    fn no_document_contains_an_answer_to_any_instance() {
        // The errata is allowed - required - to give the values. It is not
        // allowed to give an answer: a document containing one lets a model
        // pattern-match its way to a point it did not earn, and puts that
        // literal string into the training data of the arm that trains on it.
        for (name, doc) in [("SPEC", SPEC), ("ERRATA", ERRATA), ("DECOY", DECOY)] {
            let flat = flatten(doc);
            for instance in instances::all() {
                let answer = instance.expected();
                assert!(
                    !flat.contains(&answer),
                    "{name} contains the answer to {}: {answer}",
                    instance.id
                );
            }
        }
    }

    #[test]
    fn the_public_spec_carries_no_secret() {
        let flat = flatten(SPEC);
        for secret in svf::secret_renderings() {
            assert!(
                !flat.contains(&flatten(&secret)),
                "the spec leaks {secret}; arm A0 would not be measuring anything"
            );
        }
    }

    #[test]
    fn the_errata_carries_every_secret() {
        // The other half of the same property: if the errata is missing a
        // value, arm A1 fails and nothing downstream can be interpreted.
        let flat = flatten(ERRATA);
        for secret in svf::secret_renderings() {
            assert!(
                flat.contains(&flatten(&secret)),
                "the errata never states {secret}; arm A1 cannot succeed"
            );
        }
    }

    #[test]
    fn the_decoy_shares_no_value_with_the_errata() {
        // A decoy that happened to share a constant would teach part of the
        // real answer, and the control arm would stop being a control.
        let flat = flatten(DECOY);
        for secret in svf::secret_renderings() {
            assert!(
                !flat.contains(&flatten(&secret)),
                "the decoy shares {secret} with the errata"
            );
        }
    }

    #[test]
    fn the_decoy_is_comparable_in_size_to_the_errata() {
        // Same shape, same register, same order of magnitude: a control that
        // is a quarter the length is also a control for "more training data".
        let (real, decoy) = (ERRATA.len() as f64, DECOY.len() as f64);
        let ratio = real.max(decoy) / real.min(decoy);
        assert!(ratio < 1.5, "errata {real} bytes vs decoy {decoy} bytes");
    }
}
