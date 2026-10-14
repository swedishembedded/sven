// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Choosing among amounts.

use sven_vocab::Amount;

/// Index of the smallest known amount; the first of equals. `None` when
/// nothing is known, so a caller falls back to what it did before.
#[must_use]
pub fn cheapest(amounts: &[Amount]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, a) in amounts.iter().enumerate() {
        if let Amount::Known(v) = a {
            if best.is_none_or(|(_, b)| *v < b) {
                best = Some((i, *v));
            }
        }
    }
    best.map(|(i, _)| i)
}
