// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven-tools-ground`: the `ground` tool - UI-element visual grounding via
//! brain's florence2 `ground` capability action, plus the local FLAG_SECURE
//! black-screen detector `UiTestMachine` (`sven-machines`) depends on for its
//! secure-screen hand-off. See `.agents/roadmap/android-ui-test.md`.
//!
//! # Embedding this tool
//!
//! [`GroundTool`] reaches a model through a [`GroundBackend`]. The default,
//! [`SubprocessGroundBackend`], runs the `brain` CLI once per call, which
//! keeps sven standalone but re-imports the checkpoint every time. A host
//! that already holds the model resident implements
//! [`GroundBackend`] itself and builds the tool with
//! [`GroundTool::with_backend`], paying the model load once for the process
//! rather than once per screen. This crate names no brain type, so doing so
//! adds no dependency here.

pub mod black_screen;
pub mod tool;

pub use tool::{GroundBackend, GroundConfig, GroundTool, SubprocessGroundBackend};
