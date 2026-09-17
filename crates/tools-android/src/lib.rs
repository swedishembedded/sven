// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Typed Android device control over ADB.
//!
//! [`AndroidTool`] exposes a narrow verb set (screenshot, tap, swipe, type,
//! key event, launch/stop an app, list packages, display/foreground-app info,
//! wait) instead of raw shell access - see `sven_hsm::ToolCapability::
//! ControlDevice`'s doc for the reasoning. [`adb`] is the low-level process
//! wrapper the tool is built on; it's public so a future UI-test machine can
//! call it directly without going through the `Tool`/`ToolCall` JSON layer.

pub mod adb;
mod tool;
pub mod ui_tree;

pub use tool::AndroidTool;
