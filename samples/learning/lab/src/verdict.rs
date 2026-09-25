// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! Who is allowed to decide that a task was solved.
//!
//! Only this module, and only by evaluating a task's declared predicates. That
//! is the point: an agent's own statement that it finished is a claim, not a
//! result, and the moment a sample can write `Outcome::solved()` by hand, the
//! difference between the two depends on nobody having taken a shortcut.
//!
//! So [`Verdict`] has no public constructor. It comes back from
//! [`PredicateSet::evaluate`] and nothing else, and an [`crate::Outcome`] that
//! counts towards a score can only be made from one. A sample cannot mark its
//! own work correct, because there is no function that would let it.
//!
//! The same reasoning applies to the other direction. A turn that failed is
//! not an unsolved task - it is an absence of evidence either way - so the
//! conversion from a turn result is written once, here, rather than at each
//! call site where `Err` could be quietly mapped to "did not solve it".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The predicates a task declares in its `family.toml` `[completion]` block.
///
/// Held as a declared set rather than a closure so a run record can name every
/// predicate that was checked, including the ones that passed - "solved" with
/// no list of what that meant is not reproducible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateSet {
    names: Vec<String>,
}

impl PredicateSet {
    /// Declare the predicates for a task. Empty is refused: a task whose
    /// completion is defined by no predicate would be solved by doing nothing,
    /// and every instance of it would score as a success forever.
    pub fn new<I, S>(names: I) -> Result<PredicateSet, &'static str>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let names: Vec<String> = names.into_iter().map(Into::into).collect();
        if names.is_empty() {
            return Err(
                "a task must declare at least one completion predicate: with none, \
                        doing nothing satisfies it",
            );
        }
        Ok(PredicateSet { names })
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Evaluate the declared predicates against `observed`.
    ///
    /// Every declared predicate must appear in `observed`. A missing one is a
    /// broken verifier, not a failed task, and is reported as such rather than
    /// being treated as `false` - silently scoring an unevaluated predicate as
    /// a failure would make a verifier bug look like a model that could not do
    /// the work.
    pub fn evaluate(&self, observed: &BTreeMap<String, bool>) -> Result<Verdict, Unevaluated> {
        let missing: Vec<String> = self
            .names
            .iter()
            .filter(|n| !observed.contains_key(*n))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(Unevaluated { missing });
        }
        let failed: Vec<String> = self
            .names
            .iter()
            .filter(|n| !observed[*n])
            .cloned()
            .collect();
        Ok(Verdict {
            checked: self.names.clone(),
            failed,
        })
    }
}

/// The verifier did not produce a value for every declared predicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unevaluated {
    pub missing: Vec<String>,
}

impl std::fmt::Display for Unevaluated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the verifier returned no value for {} declared predicate(s): {}. That is a broken \
             verifier, not a failed task - scoring it as a failure would make a harness bug look \
             like a model that could not do the work.",
            self.missing.len(),
            self.missing.join(", ")
        )
    }
}

/// What the verifier decided, and on what basis.
///
/// Deliberately has no public constructor: see the module documentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    checked: Vec<String>,
    failed: Vec<String>,
}

impl Verdict {
    /// Solved means every declared predicate held. There is no partial credit
    /// and no threshold: a task the sample declared in full is either done or
    /// it is not.
    pub fn solved(&self) -> bool {
        self.failed.is_empty()
    }

    /// The predicates that did not hold. Kept so a failure says which part of
    /// the request went unmet, rather than only that something did.
    pub fn failed(&self) -> &[String] {
        &self.failed
    }

    /// Every predicate that was evaluated, passing or not.
    pub fn checked(&self) -> &[String] {
        &self.checked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn set() -> PredicateSet {
        PredicateSet::new(["records_present", "no_duplicates", "others_unchanged"]).expect("valid")
    }

    #[test]
    fn solved_requires_every_declared_predicate() {
        let all = observed(&[
            ("records_present", true),
            ("no_duplicates", true),
            ("others_unchanged", true),
        ]);
        assert!(set().evaluate(&all).expect("evaluated").solved());

        let one_short = observed(&[
            ("records_present", true),
            ("no_duplicates", true),
            ("others_unchanged", false),
        ]);
        let v = set().evaluate(&one_short).expect("evaluated");
        assert!(!v.solved(), "a task is not partly solved");
        assert_eq!(
            v.failed(),
            ["others_unchanged"],
            "and it names what went unmet"
        );
    }

    #[test]
    fn a_verdict_records_everything_it_checked_not_only_the_failures() {
        let all = observed(&[
            ("records_present", true),
            ("no_duplicates", true),
            ("others_unchanged", true),
        ]);
        let v = set().evaluate(&all).expect("evaluated");
        assert_eq!(
            v.checked().len(),
            3,
            "\"solved\" with no list of what that meant is not reproducible"
        );
    }

    #[test]
    fn an_unevaluated_predicate_is_a_broken_verifier_not_a_failed_task() {
        let partial = observed(&[("records_present", true), ("no_duplicates", true)]);
        let err = set()
            .evaluate(&partial)
            .expect_err("a missing predicate must not score");
        assert_eq!(err.missing, ["others_unchanged"]);
        assert!(err.to_string().contains("broken verifier"), "{err}");
    }

    #[test]
    fn a_task_with_no_predicates_is_refused() {
        // Otherwise doing nothing satisfies it, and every instance passes.
        let empty: [&str; 0] = [];
        assert!(PredicateSet::new(empty).is_err());
    }
}
