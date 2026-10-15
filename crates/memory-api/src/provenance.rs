// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Where a source came from and what it may later be used for.

use serde::{Deserialize, Serialize};

/// How a source entered memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Origin {
    /// Stated or supplied by a person.
    User,
    /// Written by an agent during the run named by `run_id`.
    Agent {
        /// The run that wrote it.
        run_id: String,
    },
    /// Imported from another system in the named `format`.
    Import {
        /// The source format, for example `markdown`.
        format: String,
    },
}

impl Origin {
    /// A short name for the origin, used to label quoted material.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::User => "user_stated",
            Self::Agent { .. } => "agent_written",
            Self::Import { .. } => "imported",
        }
    }

    /// Whether text from this origin is retrieved as untrusted material.
    ///
    /// Only what a person stated is trusted. What an agent wrote may itself
    /// have been derived from hostile content, and an import carries whatever
    /// its source held.
    #[must_use]
    pub fn is_untrusted(&self) -> bool {
        !matches!(self, Self::User)
    }
}

/// Whether the source may be used to train a model.
///
/// Carried and never enforced here: the hosts and the learning side that
/// consume memory enforce it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrainingUse {
    /// The owner agreed.
    Allowed,
    /// The owner refused.
    Denied,
    /// Nobody decided.
    #[default]
    Unspecified,
}

/// The recorded provenance of a source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// How it entered memory.
    pub origin: Origin,
    /// When it was captured, in Unix seconds as supplied by the host.
    pub captured_at: u64,
    /// Whether it may be used for training.
    pub training_use: TrainingUse,
}
