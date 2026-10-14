// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The vocabulary of cost: an amount that may be unknown, and the records a
//! request carries of what cost-aware decisions concluded and did.
//!
//! The arithmetic that produces these lives in `sven-cost`; this module holds
//! only the data, so that the request ledger can carry it.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A price-derived figure: a number in the currency the prices were declared
/// in, or unknown because a price it depends on was never declared.
///
/// Unknown is not zero. It absorbs every sum it takes part in, and
/// `sven_cost::cheapest` never returns it.
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

/// What kind of context rebuild a cache-aware decision was about. Each one
/// rewrites the prompt from some point on and so invalidates the provider's
/// cached prefix behind that point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RebuildKind {
    /// Older tool results shown in a smaller form.
    Demotion,
    /// The history replaced by a summary before the window is full.
    Compaction,
    /// A resumed session's history replaced by a recap.
    Reconstruction,
}

/// What the cost comparison favours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    /// Leaving the prompt as it is costs less over the remaining requests.
    Keep,
    /// Rebuilding costs less over the remaining requests.
    Rebuild,
    /// A price the comparison needs is not declared.
    Unknown,
}

/// What was done about a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateAction {
    Rebuilt,
    Skipped,
}

/// Why the gate did what it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateReason {
    /// `cost.cache_aware` is off: nothing was weighed.
    NotEvaluated,
    /// `advise`: the recommendation is recorded and the rebuild went ahead.
    Advisory,
    /// The prompt would not fit the model's window without it.
    RequiredToFit,
    /// `enforce`: the rebuild breaks even over the remaining requests.
    BreaksEven,
    /// `enforce`: the rebuild would not earn back the cache it discards.
    DoesNotBreakEven,
    /// `enforce`: a price is undeclared, so cost cannot justify skipping.
    PriceUnknown,
}

/// One cache-aware rebuild decision, with the numbers behind it. Costs are
/// over the remaining requests, prompt side only (what the two options do
/// not share).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RebuildRecord {
    pub kind: RebuildKind,
    pub recommendation: Recommendation,
    pub action: GateAction,
    pub reason: GateReason,
    /// Prompt size before the rebuild.
    pub prompt_tokens: u32,
    /// Tokens the rebuild removes from every later request.
    pub tokens_saved: u32,
    /// Tokens before the first one the rebuild changes; the cached prefix
    /// behind this point survives.
    pub first_changed_token: u32,
    /// Prefix tokens expected to be in the provider's cache.
    pub warm_tokens: u32,
    /// Requests the comparison was made over.
    pub remaining_requests: u32,
    pub keep: Amount,
    pub rebuild: Amount,
    /// `keep - rebuild`: positive when rebuilding is cheaper.
    pub saving: Amount,
    /// Requests after which rebuilding has earned back what it costs; absent
    /// when it never does or a price is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_even_requests: Option<f64>,
}

/// What a request's cost-aware decisions concluded and did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CostRecord {
    /// Rebuilds weighed for this request, in the order they were considered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rebuilds: Vec<RebuildRecord>,
}

impl CostRecord {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rebuilds.is_empty()
    }
}
