// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The statistics: intervals over pairs, medians next to means, and no claim
//! from a single pair.

use sven_ablation::stats::{geometric_mean_ratio, mean, median, paired_mean};

#[test]
fn one_pair_or_none_gives_no_interval() {
    assert!(paired_mean(&[], 1, "x").is_none());
    assert!(paired_mean(&[3.0], 1, "x").is_none());
    assert!(geometric_mean_ratio(&[2.0], 1, "x").is_none());
}

#[test]
fn a_consistent_difference_has_an_interval_that_excludes_zero() {
    let diffs = [2.0, 3.0, 2.5, 3.5, 2.0, 3.0];
    let i = paired_mean(&diffs, 7, "tokens").unwrap();
    assert!(i.above_zero() && i.excludes_zero());
    assert!(i.lower <= i.estimate && i.estimate <= i.upper);
    assert_eq!(i.n, 6);
}

#[test]
fn a_noisy_difference_around_zero_does_not_exclude_zero() {
    let diffs = [-3.0, 4.0, -2.0, 3.0, -1.0, 1.0, -4.0, 2.0];
    let i = paired_mean(&diffs, 7, "noise").unwrap();
    assert!(!i.excludes_zero(), "{i:?}");
}

#[test]
fn identical_runs_give_a_degenerate_interval_at_zero() {
    let i = paired_mean(&[0.0; 5], 1, "same").unwrap();
    assert_eq!((i.lower, i.estimate, i.upper), (0.0, 0.0, 0.0));
    assert!(!i.excludes_zero());
}

#[test]
fn the_interval_is_reproducible_and_depends_on_the_stream() {
    let diffs = [1.0, 5.0, 2.0, 8.0, 3.0];
    assert_eq!(paired_mean(&diffs, 9, "a"), paired_mean(&diffs, 9, "a"));
    assert_ne!(paired_mean(&diffs, 9, "a"), paired_mean(&diffs, 9, "b"));
}

#[test]
fn median_sits_next_to_the_mean_and_resists_an_outlier() {
    let xs = [1.0, 2.0, 3.0, 4.0, 100.0];
    assert_eq!(median(&xs), Some(3.0));
    assert_eq!(mean(&xs), Some(22.0));
    assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
    assert_eq!(median(&[]), None);
}

#[test]
fn ratios_are_averaged_geometrically() {
    // 2x and 0.5x cancel; an arithmetic mean of 1.25 would claim a saving.
    let i = geometric_mean_ratio(&[2.0, 0.5], 3, "r").unwrap();
    assert!((i.estimate - 1.0).abs() < 1e-9);
    assert!(geometric_mean_ratio(&[2.0, 0.0], 3, "r").is_none());
    assert!(geometric_mean_ratio(&[2.0, f64::NAN], 3, "r").is_none());
}
