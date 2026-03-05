// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Memory-mapped context tools implementing the Recursive Language Model (RLM)
//! pattern.
//!
//! Large content is kept outside the LLM context window.  The model receives a
//! symbolic handle and interacts with the content through structured tools:
//!
//! - [`ContextOpenTool`] — opens a file or directory and returns a handle + metadata
//! - [`ContextReadTool`] — random-access line-range read from a handle
//! - [`ContextGrepTool`] — regex pre-filter search over a handle
//!
//! The `context_query` and `context_reduce` tools live in `sven-bootstrap`
//! because they need access to [`sven_model::ModelProvider`] for sub-queries.
//! The [`SubQueryRunner`] trait defined here lets `sven-tools` hold a
//! reference to the runner without a direct dependency on `sven-model`.

pub mod grep;
pub mod open;
pub mod query_runner;
pub mod read;
pub mod store;

pub use grep::ContextGrepTool;
pub use open::ContextOpenTool;
pub use query_runner::SubQueryRunner;
pub use read::ContextReadTool;
pub use store::ContextStore;
