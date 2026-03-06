// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Streaming output buffer tools.
//!
//! These tools implement the read-side of the buffer abstraction.  Buffers
//! are created by the `task` and (future) `shell` tools as they stream
//! subprocess output.  The model uses these tools to inspect results without
//! loading the full output into the context window:
//!
//! - [`BufStatusTool`] (`buf_status`) — poll status, line count, elapsed time
//! - [`BufReadTool`]   (`buf_read`)   — read a specific line range
//! - [`BufGrepTool`]   (`buf_grep`)   — regex search over the buffer
//!
//! All three tools share one [`OutputBufferStore`] per session via
//! `Arc<Mutex<OutputBufferStore>>`.

pub mod grep;
pub mod read;
pub mod status;
pub mod store;

pub use grep::BufGrepTool;
pub use read::BufReadTool;
pub use status::BufStatusTool;
pub use store::{BufferSource, BufferStatus, OutputBuffer, OutputBufferStore};
