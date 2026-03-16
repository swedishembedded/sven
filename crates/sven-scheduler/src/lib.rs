// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Cron scheduler, heartbeat, and persistent job store for sven agents.
//!
//! # Overview
//!
//! This crate provides three building blocks:
//!
//! - [`JobStore`] — YAML-backed persistence for cron/interval/one-shot jobs.
//! - [`Scheduler`] — tokio task that evaluates schedules and emits [`JobDue`] events.
//! - [`Heartbeat`] — configurable periodic agent wakeup with a standing prompt.
//!
//! The [`ScheduleTool`] lets the agent create, list, and delete jobs at runtime.
//!
//! # Quick start
//!
//! ```no_run
//! use sven_scheduler::{JobStore, Scheduler};
//! use std::sync::Arc;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let store = Arc::new(JobStore::load_or_default(None)?);
//! let (tx, mut rx) = tokio::sync::mpsc::channel(16);
//!
//! let scheduler = Scheduler::new(store.clone(), tx);
//! scheduler.start().await;
//!
//! while let Some(due) = rx.recv().await {
//!     println!("Job due: {} — {}", due.job_id, due.prompt);
//! }
//! # Ok(())
//! # }
//! ```

pub mod heartbeat;
pub mod job;
pub mod scheduler;
pub mod store;
pub mod tool;

pub use heartbeat::Heartbeat;
pub use job::{Job, JobId, Schedule};
pub use scheduler::{JobDue, Scheduler};
pub use store::JobStore;
pub use tool::ScheduleTool;
