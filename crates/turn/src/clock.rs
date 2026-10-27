// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The clock a turn reads and waits on.
//!
//! Natively it is tokio's, so a test that pauses tokio's clock pauses the
//! turn's stall detection with it. In a browser it is the kernel's portable
//! runtime: the browser's own clock and timers.

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use tokio::time::{sleep_until, Instant};

#[cfg(target_arch = "wasm32")]
pub(crate) use sven_kernel::rt::{sleep_until, Instant};
