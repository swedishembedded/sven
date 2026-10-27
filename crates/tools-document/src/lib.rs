// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `document_locate` and `document_convert` tools over `sven-document`,
//! and their configuration.

mod locate;
mod models;
mod tool;

pub use locate::DocumentLocateTool;
pub use models::{NoOcrModel, OcrModel, OcrModels};
pub use tool::DocumentConvertTool;
