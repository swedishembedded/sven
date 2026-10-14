// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! An amount of money that may not be known.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A price-derived figure: a number in the currency the prices were declared
/// in, or unknown because a price it depends on was never declared.
///
/// Unknown is not zero. It absorbs every sum it takes part in, and
/// [`cheapest`] never returns it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Amount {
    Known(f64),
    Unknown,
}

impl Amount {
    pub const ZERO: Self = Self::Known(0.0);

    /// The figure, or `None` when unknown.
    #[must_use]
    pub fn value(self) -> Option<f64> {
        match self {
            Self::Known(v) => Some(v),
            Self::Unknown => None,
        }
    }

    /// The sum, unknown when either side is.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Known(a), Self::Known(b)) => Self::Known(a + b),
            _ => Self::Unknown,
        }
    }

    /// The product with a factor, unknown when this is.
    #[must_use]
    pub fn times(self, factor: f64) -> Self {
        match self {
            Self::Known(a) => Self::Known(a * factor),
            Self::Unknown => Self::Unknown,
        }
    }

    /// `self - other`, unknown when either side is.
    #[must_use]
    pub fn minus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Known(a), Self::Known(b)) => Self::Known(a - b),
            _ => Self::Unknown,
        }
    }
}

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

impl Serialize for Amount {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Known(v) => s.serialize_f64(*v),
            Self::Unknown => s.serialize_str("unknown"),
        }
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Amount;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or the word \"unknown\"")
            }
            fn visit_f64<E>(self, v: f64) -> Result<Amount, E> {
                Ok(Amount::Known(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Amount, E> {
                Ok(Amount::Known(v as f64))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Amount, E> {
                Ok(Amount::Known(v as f64))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Amount, E> {
                if v == "unknown" {
                    Ok(Amount::Unknown)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(v), &self))
                }
            }
        }
        d.deserialize_any(V)
    }
}
