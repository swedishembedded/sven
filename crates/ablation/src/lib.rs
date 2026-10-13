// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The ablation benchmark: controlled variants of the harness run over the
//! same seeded tasks, scored objectively. See
//! `docs/technical/ablation-benchmarks.md`.
//!
//! * [`matrix`]: the variant definitions and how each is realized through
//!   configuration alone.
//! * [`providers`]: the decision providers that stand in for a model.

pub mod matrix;
pub mod providers;
