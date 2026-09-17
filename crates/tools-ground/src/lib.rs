// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven-tools-ground`: the `ground` tool - UI-element visual grounding via
//! a vision model's `ground` capability. The FLAG_SECURE black-screen check
//! it applies before sending any frame to a model lives in `sven-image`
//! (`sven_image::is_flag_secure_black`), since driving a secure screen is a
//! concern of every device path, not only this one. See
//! `.agents/roadmap/android-ui-test.md`.
//!
//! # Embedding this tool
//!
//! [`GroundTool`] reaches a model through a [`GroundBackend`]. The default,
//! [`SubprocessGroundBackend`], invokes a configurable grounding CLI once
//! per call, which keeps sven standalone but re-imports the checkpoint every
//! time. A host that already holds the model resident implements
//! [`GroundBackend`] itself and builds the tool with
//! [`GroundTool::with_backend`], paying the model load once for the process
//! rather than once per screen. This crate depends on no model
//! implementation and names no vendor type, so doing so adds no dependency
//! here.

pub mod tool;

pub use tool::{GroundBackend, GroundConfig, GroundTool, SubprocessGroundBackend};
