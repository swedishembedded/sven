// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Generated catalogs and queries for measuring tool search, and the metrics.

pub mod corpus;
pub mod lexicon;
pub mod metrics;
pub mod rng;

use corpus::QueryKind;

/// The held-out recall@8 of the ranker before any quality work, floored a
/// little under what was measured so the pin catches regressions, not noise.
pub const BASELINE_RECALL_FLOOR: [(QueryKind, f64); 3] = [
    (QueryKind::Exact, 0.98),
    (QueryKind::Paraphrase, 0.55),
    (QueryKind::Indirect, 0.05),
];
