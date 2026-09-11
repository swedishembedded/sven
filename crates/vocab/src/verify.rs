// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Declarative, programmatic task-success predicates.
//!
//! [`VerifierSpec`] is pure data describing how to check whether a task was
//! actually accomplished - the missing input [`sven_session_model::Verdict`]
//! needs before a claimed success can be scored (see that crate's
//! `outcome` module). Evaluating a spec is impure (it reads files, makes
//! HTTP requests), so it does not happen here; this crate only names what to
//! check.
//!
//! v1 is declarative-only by design: no `Command`/`UnitTests` shape exists
//! yet, so there is no arbitrary-code-execution surface at all in this
//! vocabulary. Every variant here is inherently safe to evaluate without
//! human approval - see the (future) capability mapping this enables.
//!
//! # Forward compatibility
//!
//! [`VerifierSpec::Unsupported`] is what makes adding a new predicate shape
//! later non-breaking: an older `sven` deserializes a spec naming a kind it
//! doesn't know into `Unsupported` rather than failing to parse, and
//! evaluating `Unsupported` always yields [`VerifierVerdict::Unknown`] -
//! **never** [`VerifierVerdict::Passed`]. A verifier an old binary can't
//! understand must never be silently treated as satisfied.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A declarative task-success predicate.
///
/// Tagged on `kind` so an unrecognized shape deserializes to
/// [`Self::Unsupported`] instead of failing to parse - see the module doc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerifierSpec {
    /// A file exists, optionally at least `min_bytes` long.
    FileExists {
        /// Path to check, relative to whatever root the evaluator resolves
        /// against.
        path: String,
        /// Minimum file size in bytes. `None` only checks existence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_bytes: Option<u64>,
    },
    /// A file exists and its contents hash to exactly `sha256`.
    FileHash {
        /// Path to check, relative to whatever root the evaluator resolves
        /// against.
        path: String,
        /// Expected SHA-256 digest, lowercase hex.
        sha256: String,
    },
    /// A JSON file, read and addressed by RFC 6901 pointer, compares true
    /// against `value` under `op`.
    JsonPredicate {
        /// Path to the JSON file, relative to whatever root the evaluator
        /// resolves against.
        path: String,
        /// RFC 6901 JSON Pointer into the parsed document (`""` = the whole
        /// document).
        pointer: String,
        /// How the pointed-to value compares against `value`.
        op: JsonCmpOp,
        /// The value to compare against.
        value: Value,
    },
    /// An HTTP request returns the expected status and/or body substring.
    HttpPredicate {
        /// URL to request (`GET`).
        url: String,
        /// Expected status code. `None` accepts any status.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_status: Option<u16>,
        /// Substring the response body must contain. `None` skips the check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_contains: Option<String>,
    },
    /// No programmatic predicate applies - ask a human.
    ///
    /// Structurally incapable of contributing a [`VerifierVerdict::Passed`]:
    /// evaluating this always yields [`VerifierVerdict::NeedsHuman`]. "Ask
    /// the user, assume yes" is exactly the self-grading escape hatch this
    /// type exists to prevent.
    AskHuman {
        /// The question to ask.
        question: String,
        /// Offered choices, if any (empty for a free-form question).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
    },
    /// Every sub-spec must pass.
    All {
        /// The sub-specs, all of which must pass.
        specs: Vec<VerifierSpec>,
    },
    /// At least one sub-spec must pass.
    Any {
        /// The sub-specs, at least one of which must pass.
        specs: Vec<VerifierSpec>,
    },
    /// A spec kind this build does not recognize.
    ///
    /// See the module doc - this is the forward-compatibility guarantee, not
    /// an error case. Constructing one directly is meaningless (there is
    /// nothing to evaluate); it only ever arises from deserializing an
    /// unrecognized `kind`.
    #[serde(other)]
    Unsupported,
}

/// Comparison operator for [`VerifierSpec::JsonPredicate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonCmpOp {
    /// Values are equal.
    Eq,
    /// Values are not equal.
    Ne,
    /// The pointed-to value, as a string, contains the given value's string.
    Contains,
}

/// The result of evaluating one [`VerifierSpec`].
///
/// Deliberately has no `Passed`-shaped path that does not require an actual
/// check to have run and succeeded - `NeedsHuman` and `Unknown` are both
/// distinct from, and must never collapse into, `Passed`.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifierVerdict {
    /// The predicate held.
    Passed,
    /// The predicate did not hold. `reason` is a human-facing diagnostic.
    Failed {
        /// Why the predicate failed.
        reason: String,
    },
    /// No programmatic predicate applies; a human must decide.
    NeedsHuman {
        /// The question to ask.
        question: String,
        /// Offered choices, if any (empty for a free-form question) - carried
        /// through so this can be handed directly to the async
        /// question-parking primitive (`Event::QuestionAsked`).
        options: Vec<String>,
    },
    /// The predicate could not be evaluated (an unsupported spec, an I/O
    /// error, ...). `reason` is a human-facing diagnostic.
    Unknown {
        /// Why no verdict could be reached.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrecognized_kind_deserializes_to_unsupported_not_an_error() {
        let json = serde_json::json!({"kind": "vision_predicate", "target": "a red button"});
        let spec: VerifierSpec = serde_json::from_value(json).expect("must deserialize, not error");
        assert_eq!(spec, VerifierSpec::Unsupported);
    }

    #[test]
    fn file_exists_round_trips_through_json() {
        let spec = VerifierSpec::FileExists {
            path: "out.txt".into(),
            min_bytes: Some(10),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["kind"], "file_exists");
        let back: VerifierSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn all_and_any_carry_nested_specs_through_json() {
        let spec = VerifierSpec::All {
            specs: vec![
                VerifierSpec::FileExists { path: "a".into(), min_bytes: None },
                VerifierSpec::Any {
                    specs: vec![VerifierSpec::FileExists { path: "b".into(), min_bytes: None }],
                },
            ],
        };
        let json = serde_json::to_value(&spec).unwrap();
        let back: VerifierSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn ask_human_carries_no_path_to_passed() {
        // Structural check on the vocabulary itself: there is no
        // constructor, no field, nothing on `VerifierSpec::AskHuman` that
        // can produce `VerifierVerdict::Passed` - only an evaluator
        // (untested here) decides that, and it must never map AskHuman to
        // Passed. This test just pins the spec's shape carries no such data.
        let spec = VerifierSpec::AskHuman {
            question: "Did it work?".into(),
            options: vec![],
        };
        match spec {
            VerifierSpec::AskHuman { question, options } => {
                assert_eq!(question, "Did it work?");
                assert!(options.is_empty());
            }
            other => panic!("expected AskHuman, got {other:?}"),
        }
    }
}
