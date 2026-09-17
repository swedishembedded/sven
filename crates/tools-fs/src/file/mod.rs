// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! File operation tools.

pub mod asr;
pub mod attach_file;
pub mod attachment;
pub mod edit_file;
pub mod find_file;
pub mod read_file;
pub mod write_file;

pub use attach_file::AttachFileTool;
pub use attachment::{
    classify as classify_attachment, load_attachment, AttachError, AttachOptions, AttachmentKind,
    LoadedAttachment,
};
pub use edit_file::EditFileTool;
pub use find_file::FindFileTool;
pub use read_file::ReadFileTool;
pub use write_file::WriteTool;
