// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Declarative, programmatic task-success predicates.
//!
//! [`VerifierSpec`] is pure data describing how to check whether a task was
//! actually accomplished - the missing input `sven_session_model::Verdict`
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
use sha2::{Digest, Sha256};

use crate::provenance::ContentDigest;

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
///
/// `Serialize`/`Deserialize` so it can travel as an [`crate::provenance`]-style
/// kernel event payload (`Event::VerificationComplete`) - event-sourcing
/// replay requires every event to round-trip through JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
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

/// Canonicalizes `value` so its JSON serialization is stable regardless of
/// object-key insertion order (recursively re-sorts every object's keys).
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

/// A content digest of `spec`, stable under key reordering.
///
/// This is the hash a [`FrozenVerifier`] pins and that `Verifying` later
/// recomputes to detect tampering - see [`FrozenVerifier`]'s doc. Pure
/// computation (no I/O), so it is safe to call from inside a machine
/// transition, not just from an executor.
#[must_use]
pub fn spec_hash(spec: &VerifierSpec) -> ContentDigest {
    let value = serde_json::to_value(spec).expect("VerifierSpec always serializes");
    let canonical = canonicalize(&value);
    let bytes = serde_json::to_vec(&canonical).expect("a canonicalized Value always serializes");
    ContentDigest::from_hex(hex::encode(Sha256::digest(&bytes)))
}

/// Where a [`FrozenVerifier`] came from.
///
/// Never constructible from a model-supplied argument - see the module doc's
/// note on `AskHuman`, and [`crate::provenance`]'s identical rule for
/// [`crate::provenance::FactSource`]. `Authored` is the only variant today:
/// a task's verifier is always written by the human/operator who authored the
/// task file, never derived from a document the agent fetched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum VerifierOrigin {
    /// Written by a human into a task file. `digest` is the content digest of
    /// that file's raw bytes, computed by whatever read it (never taken from
    /// a caller-supplied argument) - so the origin is traceable to exactly
    /// which bytes produced this verifier, not to a runtime claim.
    Authored {
        /// Content digest of the task file's raw bytes.
        digest: ContentDigest,
    },
}

/// A [`VerifierSpec`] pinned at the moment a task was frozen, with the hash
/// that lets `Verifying` detect if it was ever rewritten.
///
/// # Why this exists
///
/// Freeze-before-attempt only means something if what got frozen cannot
/// quietly change before it is used to grade. `spec_hash` is recomputed from
/// `spec` at verification time and compared to the hash stored here at freeze
/// time; today nothing in the attempt loop can touch a frozen fact (no tool
/// writes `Context` facts directly), so a mismatch should never occur - but
/// the check is defense in depth against a future bug reintroducing that
/// possibility, not decoration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenVerifier {
    /// The pinned predicate.
    pub spec: VerifierSpec,
    /// Content digest of `spec`'s canonical JSON, computed once at freeze time.
    pub spec_hash: ContentDigest,
    /// Who is answerable for this verifier existing.
    pub origin: VerifierOrigin,
}

impl FrozenVerifier {
    /// Freezes `spec`, computing its hash now.
    #[must_use]
    pub fn freeze(spec: VerifierSpec, origin: VerifierOrigin) -> Self {
        let spec_hash = spec_hash(&spec);
        Self {
            spec,
            spec_hash,
            origin,
        }
    }

    /// `true` if `spec`'s current hash still matches the one pinned at freeze
    /// time - see the struct doc.
    #[must_use]
    pub fn still_matches(&self) -> bool {
        spec_hash(&self.spec) == self.spec_hash
    }
}

/// A unit of agentic work with a verifier pinned before any attempt starts.
///
/// Pure data - authored by a human (typically as a `.task.toml` file, parsed
/// at the impure CLI/wiring boundary, never inside a machine transition) and
/// handed to the verified-task machine already complete. The machine itself
/// never invents a `Task`; it only ever receives one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Stable identifier, used to key per-task splits/retention (e.g. Stage 6's
    /// curator) and to label episode provenance.
    pub id: String,
    /// The instruction shown to the model attempting this task.
    pub prompt: String,
    /// How a claimed completion is checked. Frozen before the first attempt -
    /// see [`FrozenVerifier`].
    pub verifier: VerifierSpec,
    /// Attempts allowed before giving up without a passing verdict.
    #[serde(default = "Task::default_max_attempts")]
    pub max_attempts: u32,
}

impl Task {
    /// Default retry budget when a task file omits `max_attempts`.
    #[must_use]
    pub const fn default_max_attempts() -> u32 {
        2
    }
}

/// The whole payload the verified-task machine's `Freeze` state parses out of
/// its first `Event::UserMessage` - a [`Task`] plus the content digest of the
/// bytes it was read from.
///
/// This is the wire shape between the impure loader (which reads the task
/// file, hashes it, and posts this as JSON) and the machine (which only ever
/// parses already-provided text - see the module's `Freeze` state doc for why
/// that keeps the transition pure).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedTaskSeed {
    /// The task to attempt.
    pub task: Task,
    /// Content digest of the raw file bytes `task` was parsed from.
    pub source_digest: ContentDigest,
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
                VerifierSpec::FileExists {
                    path: "a".into(),
                    min_bytes: None,
                },
                VerifierSpec::Any {
                    specs: vec![VerifierSpec::FileExists {
                        path: "b".into(),
                        min_bytes: None,
                    }],
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

    #[test]
    fn spec_hash_is_stable_under_key_reordering() {
        // The same predicate, serialized with fields in a different order,
        // must hash identically - otherwise a harmless refactor of the
        // serializer would look like tampering to `FrozenVerifier::still_matches`.
        let a = VerifierSpec::FileExists {
            path: "out.txt".into(),
            min_bytes: Some(1),
        };
        let value = serde_json::to_value(&a).unwrap();
        let reordered = serde_json::json!({
            "min_bytes": value["min_bytes"],
            "path": value["path"],
            "kind": value["kind"],
        });
        let b: VerifierSpec = serde_json::from_value(reordered).unwrap();
        assert_eq!(a, b, "sanity: still the same spec");
        assert_eq!(spec_hash(&a), spec_hash(&b));
    }

    #[test]
    fn spec_hash_differs_for_different_specs() {
        let a = VerifierSpec::FileExists {
            path: "a".into(),
            min_bytes: None,
        };
        let b = VerifierSpec::FileExists {
            path: "b".into(),
            min_bytes: None,
        };
        assert_ne!(spec_hash(&a), spec_hash(&b));
    }

    #[test]
    fn frozen_verifier_still_matches_until_the_spec_changes() {
        let spec = VerifierSpec::FileExists {
            path: "out.txt".into(),
            min_bytes: None,
        };
        let frozen = FrozenVerifier::freeze(
            spec,
            VerifierOrigin::Authored {
                digest: ContentDigest::from_hex("deadbeef"),
            },
        );
        assert!(frozen.still_matches());

        let mut tampered = frozen.clone();
        tampered.spec = VerifierSpec::FileExists {
            path: "different.txt".into(),
            min_bytes: None,
        };
        assert!(
            !tampered.still_matches(),
            "a spec that no longer matches its pinned hash must be detectable"
        );
    }

    #[test]
    fn task_without_max_attempts_defaults_to_two() {
        let json = serde_json::json!({
            "id": "t1",
            "prompt": "do the thing",
            "verifier": {"kind": "file_exists", "path": "out.txt"},
        });
        let task: Task = serde_json::from_value(json).unwrap();
        assert_eq!(task.max_attempts, 2);
    }

    #[test]
    fn verified_task_seed_round_trips_through_json() {
        let seed = VerifiedTaskSeed {
            task: Task {
                id: "t1".into(),
                prompt: "do the thing".into(),
                verifier: VerifierSpec::FileExists {
                    path: "out.txt".into(),
                    min_bytes: None,
                },
                max_attempts: 3,
            },
            source_digest: ContentDigest::from_hex("abc123"),
        };
        let json = serde_json::to_value(&seed).unwrap();
        let back: VerifiedTaskSeed = serde_json::from_value(json).unwrap();
        assert_eq!(seed, back);
    }
}
