// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Honest statistics for paired comparisons.
//!
//! The unit of replication is one task at one seed run under two variants: a
//! pair. A run's turns, requests or tool calls are not independent samples
//! and are never resampled. Intervals are percentile bootstrap intervals over
//! pairs with a fixed seed, so a report is reproducible byte for byte.
//! Fewer than two pairs give no interval: nothing is claimed from one sample.

use serde::{Deserialize, Serialize};

use crate::rng::Rng;

/// Resamples drawn for every interval.
pub const RESAMPLES: usize = 2000;
/// Coverage of every interval.
pub const CONFIDENCE: f64 = 0.95;

/// A point estimate with a bootstrap interval over `n` pairs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    pub estimate: f64,
    pub lower: f64,
    pub upper: f64,
    pub n: usize,
}

impl Interval {
    /// The interval lies entirely above zero.
    #[must_use]
    pub fn above_zero(&self) -> bool {
        self.lower > 0.0
    }

    /// The interval lies entirely below zero.
    #[must_use]
    pub fn below_zero(&self) -> bool {
        self.upper < 0.0
    }

    /// The interval does not contain zero.
    #[must_use]
    pub fn excludes_zero(&self) -> bool {
        self.above_zero() || self.below_zero()
    }
}

#[must_use]
pub fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

#[must_use]
pub fn median(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    })
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    let position = q * (sorted.len() - 1) as f64;
    let (low, high) = (position.floor() as usize, position.ceil() as usize);
    sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
}

/// The mean of `diffs` (one per pair) with a percentile bootstrap interval;
/// `None` for fewer than two pairs. `stream` names what is being estimated,
/// so two estimates of one report do not share a resampling sequence.
#[must_use]
pub fn paired_mean(diffs: &[f64], seed: u64, stream: &str) -> Option<Interval> {
    if diffs.len() < 2 {
        return None;
    }
    let mut rng = Rng::new(seed, stream);
    let n = diffs.len();
    let mut means: Vec<f64> = (0..RESAMPLES)
        .map(|_| (0..n).map(|_| diffs[rng.below(n)]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(f64::total_cmp);
    let tail = (1.0 - CONFIDENCE) / 2.0;
    Some(Interval {
        estimate: mean(diffs)?,
        lower: percentile(&means, tail),
        upper: percentile(&means, 1.0 - tail),
        n,
    })
}

/// The geometric mean of per-pair ratios with a bootstrap interval, computed
/// on the logarithms. A ratio must be positive and finite; `None` otherwise
/// or for fewer than two pairs.
#[must_use]
pub fn geometric_mean_ratio(ratios: &[f64], seed: u64, stream: &str) -> Option<Interval> {
    if ratios.iter().any(|r| !(r.is_finite() && *r > 0.0)) {
        return None;
    }
    let logs: Vec<f64> = ratios.iter().map(|r| r.ln()).collect();
    let log = paired_mean(&logs, seed, stream)?;
    Some(Interval {
        estimate: log.estimate.exp(),
        lower: log.lower.exp(),
        upper: log.upper.exp(),
        n: log.n,
    })
}
