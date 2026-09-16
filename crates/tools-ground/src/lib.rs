// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven-tools-ground`: the `ground` tool - UI-element visual grounding via
//! brain's florence2 `ground` capability action, plus the local FLAG_SECURE
//! black-screen detector `UiTestMachine` (`sven-machines`) depends on for its
//! secure-screen hand-off. See `.agents/roadmap/android-ui-test.md`.

pub mod black_screen;
pub mod tool;

pub use tool::{GroundConfig, GroundTool};
