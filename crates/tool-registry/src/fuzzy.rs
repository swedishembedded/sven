// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Near-miss matching of names, for a query that spells a name almost right.

/// The distinct character trigrams of `text`, padded by a space on each side,
/// as sorted hashes.
pub fn trigrams(text: &str) -> Vec<u32> {
    let padded: Vec<char> = format!(" {text} ").chars().collect();
    let mut grams: Vec<u32> = padded
        .windows(3)
        .map(|w| {
            w.iter().fold(0x811c_9dc5u32, |hash, c| {
                (hash ^ u32::from(*c)).wrapping_mul(0x0100_0193)
            })
        })
        .collect();
    grams.sort_unstable();
    grams.dedup();
    grams
}

/// The Sørensen-Dice coefficient of two sorted trigram sets: 0 for nothing in
/// common, 1 for the same set.
pub fn dice(a: &[u32], b: &[u32]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut shared) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                i += 1;
                j += 1;
            }
        }
    }
    2.0 * shared as f64 / (a.len() + b.len()) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_share_most_trigrams_are_close() {
        let a = trigrams("shipping parcel");
        assert!(dice(&a, &trigrams("shiping parcel")) > 0.8);
        assert!(dice(&a, &trigrams("garden sprinkler")) < 0.2);
        assert_eq!(dice(&a, &a), 1.0);
    }
}
