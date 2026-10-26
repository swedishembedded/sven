// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `document_convert` tool over `sven-document`, and its configuration.

mod models;
mod tool;

pub use models::{NoOcrModel, OcrModel, OcrModels};
pub use tool::DocumentConvertTool;
