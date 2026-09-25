// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! What an arm measured, and what it refuses to claim.
//!
//! One rule carries most of the weight here: an instance that never reached a
//! verdict - a transport fault, a server that went away, a budget spent
//! before the model answered - is counted as an [`Outcome::Errored`] and
//! excluded from BOTH the numerator and the denominator. A run that lost half
//! its instances to an infrastructure fault would otherwise report a low score
//! indistinguishable from a model that did not know the answer, and the two
//! call for opposite responses.
//!
//! An arm that errored at all is not silently comparable with one that did
//! not, so [`ArmScore::comparable`] says so and the report prints it.

use serde::{Deserialize, Serialize};

/// What happened on one instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Outcome {
    /// The instance reached a verdict. `solved` is the verifier's, never the
    /// agent's own opinion of whether it finished.
    Answered { solved: bool },
    /// The instance never reached a verdict. `reason` is kept so a run can be
    /// diagnosed without re-running it.
    Errored { reason: String },
}

impl Outcome {
    pub fn solved() -> Outcome {
        Outcome::Answered { solved: true }
    }
    pub fn unsolved() -> Outcome {
        Outcome::Answered { solved: false }
    }
    pub fn errored(reason: impl Into<String>) -> Outcome {
        Outcome::Errored { reason: reason.into() }
    }
}

/// The score of one arm: one model, one configuration, one set of instances.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmScore {
    pub label: String,
    /// The model this arm actually ran against, recorded so a number can never
    /// be attributed to the wrong weights. See [`crate::ServedModel`].
    pub model: String,
    pub solved: usize,
    pub answered: usize,
    pub errored: usize,
}

impl ArmScore {
    pub fn new(label: impl Into<String>, model: impl Into<String>) -> ArmScore {
        ArmScore { label: label.into(), model: model.into(), ..ArmScore::default() }
    }

    pub fn record(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Answered { solved } => {
                self.answered += 1;
                self.solved += usize::from(*solved);
            }
            Outcome::Errored { .. } => self.errored += 1,
        }
    }

    /// Instances that reached a verdict. This is the denominator - NOT the
    /// number of instances attempted.
    pub fn denominator(&self) -> usize {
        self.answered
    }

    /// `None` when nothing reached a verdict: there is no score to report, and
    /// reporting 0.0 would be a claim about the model that was never measured.
    pub fn rate(&self) -> Option<f64> {
        (self.answered > 0).then(|| self.solved as f64 / self.answered as f64)
    }

    /// Whether this arm may be compared with another. An arm that lost
    /// instances to infrastructure is not comparable until it is re-run.
    pub fn comparable(&self) -> bool {
        self.errored == 0
    }

    /// The warning a report must print when this arm is not comparable.
    pub fn caveat(&self) -> Option<String> {
        (!self.comparable()).then(|| {
            format!(
                "WARNING: {} of {} instances in arm {:?} never reached a verdict; \
                 this arm is not comparable with another until they are re-run.",
                self.errored,
                self.errored + self.answered,
                self.label
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm_with(solved: usize, unsolved: usize, errored: usize) -> ArmScore {
        let mut a = ArmScore::new("t", "brain/qwen3");
        for _ in 0..solved {
            a.record(&Outcome::solved());
        }
        for _ in 0..unsolved {
            a.record(&Outcome::unsolved());
        }
        for _ in 0..errored {
            a.record(&Outcome::errored("transport"));
        }
        a
    }

    #[test]
    fn an_errored_instance_leaves_both_the_numerator_and_the_denominator_alone() {
        let clean = arm_with(3, 1, 0);
        let faulted = arm_with(3, 1, 6);
        assert_eq!(clean.rate(), Some(0.75));
        assert_eq!(faulted.rate(), Some(0.75), "errors must not depress the score");
        assert_eq!(faulted.denominator(), 4, "the denominator is verdicts, not attempts");
    }

    #[test]
    fn an_arm_that_errored_says_it_is_not_comparable() {
        assert!(arm_with(3, 1, 0).comparable());
        assert!(arm_with(3, 1, 0).caveat().is_none());
        let faulted = arm_with(3, 1, 6);
        assert!(!faulted.comparable());
        assert!(faulted.caveat().expect("a caveat").contains("not comparable"));
    }

    #[test]
    fn an_arm_where_nothing_answered_has_no_score_rather_than_a_zero() {
        // Reporting 0.0 here would state a measured failure that never
        // happened: the model was never successfully asked.
        assert_eq!(arm_with(0, 0, 5).rate(), None);
    }
}
