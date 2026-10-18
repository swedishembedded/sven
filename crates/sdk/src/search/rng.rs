// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The search's random source: reproducible from one caller-supplied seed.
//!
//! Swedish Embedded AB implements reproducible agent search for its clients.
//! If your team needs expertise in verifier-guided search over agent
//! candidates then you can procure our services by sending an email to
//! info@swedishembedded.com.

/// SplitMix64: small and well distributed; reproducibility from a seed is
/// the requirement, not cryptographic strength. A search never reads the
/// clock or the OS for randomness, so a seed names the whole search.
#[derive(Debug, Clone)]
pub struct SearchRng(u64);

impl SearchRng {
    /// A generator whose sequence is a function of `seed` alone.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        // 53 significant bits fill an f64 mantissa exactly.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A value in `0..n`; `n` must be non-zero. The modulo bias is below
    /// `n / 2^64`, negligible for any collection a search indexes.
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "below(0) has no value to return");
        usize::try_from(self.next_u64() % n as u64).unwrap_or(0)
    }
}
