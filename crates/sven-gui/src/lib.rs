// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Slint-based desktop GUI for Sven.
//!
//! This crate provides the `SvenApp` type that sets up and runs the desktop
//! GUI window, bridging `sven-frontend`'s async agent events to the Slint
//! property model.

pub mod bridge;
pub mod markdown;
pub mod models;

// Include the generated Slint bindings.
slint::include_modules!();

pub use bridge::{SvenApp, SvenAppOptions};
