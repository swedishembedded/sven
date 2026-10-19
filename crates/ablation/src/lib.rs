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
//! `docs/contributing/ablation-benchmarks.md`.
//!
//! * [`matrix`]: the variant definitions and how each is realized through
//!   configuration alone.
//! * [`providers`]: the decision providers that stand in for a model.
//! * [`task`] and [`suites`]: seeded task generators, the same for every
//!   variant.
//! * [`mockscript`]: a task's scripted model.
//! * [`evidence`] and [`score`]: what a model was sent and the objective
//!   verdict on a run.
//! * [`mcp_stub`]: the tool catalog server of the tool-selection suite.

pub mod answer;
pub mod evidence;
pub mod export;
pub mod gates;
pub mod loader;
pub mod markdown;
pub mod matrix;
pub mod mcp_stub;
pub mod mockscript;
pub mod plan;
pub mod provider_server;
pub mod providers;
pub mod real_report;
pub mod record;
pub mod recovery;
pub mod report;
pub mod report_recovery;
pub mod rng;
pub mod runner;
pub mod score;
pub mod stats;
pub mod suites;
pub mod task;
pub mod trajectory;
