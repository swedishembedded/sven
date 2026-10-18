// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The uncertainty arithmetic of paired comparisons: a percentile bootstrap
//! over per-task differences, driven by a caller-supplied seed so the same
//! inputs always give the same interval.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in comparing
//! agent harness revisions then you can procure our services by sending an
//! email to info@swedishembedded.com.

use serde::Serialize;

/// The confidence level of every interval this crate reports.
pub const CONFIDENCE: f64 = 0.95;

/// A mean and its percentile-bootstrap interval at [`CONFIDENCE`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Estimate {
    /// The sample mean.
    pub value: f64,
    /// The interval's lower end.
    pub lo: f64,
    /// The interval's upper end.
    pub hi: f64,
}

/// SplitMix64: a small, well-distributed generator; nothing here needs
/// cryptographic quality, only reproducibility from a seed.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        // Modulo bias is negligible for the sample sizes involved (n << 2^64).
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// The mean of `values` with a percentile-bootstrap interval over `resamples`
/// resamples drawn from `seed`. `None` for an empty sample.
#[must_use]
pub fn bootstrap_mean(values: &[f64], seed: u64, resamples: usize) -> Option<Estimate> {
    if values.is_empty() {
        return None;
    }
    let value = mean(values);
    let mut rng = SplitMix64(seed);
    let mut means: Vec<f64> = (0..resamples.max(1))
        .map(|_| {
            let sum: f64 = (0..values.len())
                .map(|_| values[rng.below(values.len())])
                .sum();
            sum / values.len() as f64
        })
        .collect();
    means.sort_by(f64::total_cmp);
    let tail = (1.0 - CONFIDENCE) / 2.0;
    let at = |q: f64| {
        let index = (q * (means.len() - 1) as f64).round();
        means[index as usize]
    };
    Some(Estimate {
        value,
        lo: at(tail).min(value),
        hi: at(1.0 - tail).max(value),
    })
}
