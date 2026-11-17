// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements reproducible, training-grade agent runs for
// its clients. If your team needs expertise in deterministic sampling across
// model servers then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The sampling controls a request can set, as names: what a provider
//! declares it honours, what a request asked for and what a record says took
//! effect.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One sampling control a request can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingControl {
    /// Softmax temperature.
    Temperature,
    /// Nucleus-sampling mass.
    TopP,
    /// Number of highest-probability tokens kept before sampling.
    TopK,
    /// Seed for deterministic sampling.
    Seed,
}

impl SamplingControl {
    /// Every control, in the order a set lists them.
    pub const ALL: [Self; 4] = [Self::Temperature, Self::TopP, Self::TopK, Self::Seed];

    /// The control's name as a request body and a record spell it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Temperature => "temperature",
            Self::TopP => "top_p",
            Self::TopK => "top_k",
            Self::Seed => "seed",
        }
    }

    /// The control a request parameter `name` sets, if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

impl std::fmt::Display for SamplingControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of sampling controls. Serialized as the list of their names, in
/// [`SamplingControl::ALL`] order.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SamplingControls(u8);

impl SamplingControls {
    /// No control.
    pub const NONE: Self = Self(0);
    /// Every control.
    pub const ALL: Self = Self(0b1111);

    /// The set of `controls`.
    #[must_use]
    pub const fn of(controls: &[SamplingControl]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < controls.len() {
            bits |= controls[i].bit();
            i += 1;
        }
        Self(bits)
    }

    /// This set with `control` in it.
    #[must_use]
    pub const fn with(self, control: SamplingControl) -> Self {
        Self(self.0 | control.bit())
    }

    /// Whether `control` is in the set.
    #[must_use]
    pub const fn contains(self, control: SamplingControl) -> bool {
        self.0 & control.bit() != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// The controls in either set.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The controls in both sets.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// The controls of this set that are not in `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// The controls in the set, in [`SamplingControl::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = SamplingControl> {
        SamplingControl::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }
}

impl FromIterator<SamplingControl> for SamplingControls {
    fn from_iter<I: IntoIterator<Item = SamplingControl>>(iter: I) -> Self {
        iter.into_iter().fold(Self::NONE, Self::with)
    }
}

impl std::fmt::Debug for SamplingControls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl std::fmt::Display for SamplingControls {
    /// The names, comma-separated.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.iter().map(SamplingControl::name).collect();
        f.write_str(&names.join(", "))
    }
}

impl Serialize for SamplingControls {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for SamplingControls {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<SamplingControl>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_serializes_as_names_in_a_fixed_order() {
        let set = SamplingControls::of(&[SamplingControl::Seed, SamplingControl::Temperature]);
        let json = serde_json::to_value(set).unwrap();
        assert_eq!(json, serde_json::json!(["temperature", "seed"]));
        assert_eq!(
            serde_json::from_value::<SamplingControls>(json).unwrap(),
            set
        );
        assert_eq!(
            SamplingControls::ALL.without(set),
            SamplingControls::of(&[SamplingControl::TopP, SamplingControl::TopK])
        );
    }
}
