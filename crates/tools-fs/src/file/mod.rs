// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>

// SPDX-License-Identifier: Apache-2.0
//! File operation tools.
//!
//! Images and audio (`attach_file`, `load_attachment`, images in `read_file`)
//! are compiled in with the `media` feature, on by default; transcribing audio
//! for a model that cannot hear (`asr`) needs the `asr` feature on Unix.

#[cfg(all(unix, feature = "asr"))]
pub mod asr;
#[cfg(feature = "media")]
pub mod attach_file;
#[cfg(feature = "media")]
pub mod attachment;
pub mod edit_file;
pub mod find_file;
pub mod read_file;
pub mod write_file;

#[cfg(feature = "media")]
pub use attach_file::AttachFileTool;
#[cfg(feature = "media")]
pub use attachment::{
    classify as classify_attachment, load_attachment, AttachError, AttachOptions, AttachmentKind,
    LoadedAttachment,
};
pub use edit_file::EditFileTool;
pub use find_file::FindFileTool;
pub use read_file::ReadFileTool;
pub use write_file::WriteTool;
