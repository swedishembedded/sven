// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Filesystem tools: direct file read/write/edit/find/attach, and the
//! streaming subprocess-output buffer tools that let the model page through
//! large command output without loading it all into the context window.
//!
//! The `file/` and `buffer/` modules do not reference each other.
//! `GrepMatch`, shared with `sven-tools-ctx`'s `context/store.rs`, lives in
//! `sven-tool-api` (kernel tier) rather than in either domain crate.
pub mod buffer;
pub mod file;

pub use buffer::{
    BufGrepTool, BufReadTool, BufStatusTool, BufferSource, BufferStatus, OutputBufferStore,
};
pub use file::{
    classify_attachment, load_attachment, AttachError, AttachFileTool, AttachOptions,
    AttachmentKind, EditFileTool, FindFileTool, LoadedAttachment, ReadFileTool, WriteTool,
};

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Pins each tool's declared `OutputCategory`: the executor's truncation
// strategy depends on it, so a silent change would change what the model
// sees of every oversized result.
#[cfg(test)]
mod output_category_tests {
    use std::sync::Arc;
    use sven_tool_api::tool::{OutputCategory, Tool};
    use tokio::sync::Mutex;

    #[test]
    fn read_file_is_filecontent() {
        let t = super::file::read_file::ReadFileTool::default();
        assert_eq!(t.output_category(), OutputCategory::FileContent);
    }

    #[test]
    fn write_tool_is_generic() {
        let t = super::file::write_file::WriteTool::default();
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn edit_file_is_generic() {
        let t = super::file::edit_file::EditFileTool::default();
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn find_file_tool_is_generic() {
        let t = super::file::find_file::FindFileTool::default();
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn buf_read_is_filecontent() {
        let store = Arc::new(Mutex::new(super::buffer::store::OutputBufferStore::new()));
        let t = super::buffer::read::BufReadTool::new(store);
        assert_eq!(t.output_category(), OutputCategory::FileContent);
    }

    #[test]
    fn buf_grep_is_matchlist() {
        let store = Arc::new(Mutex::new(super::buffer::store::OutputBufferStore::new()));
        let t = super::buffer::grep::BufGrepTool::new(store);
        assert_eq!(t.output_category(), OutputCategory::MatchList);
    }

    #[test]
    fn buf_status_is_generic() {
        let store = Arc::new(Mutex::new(super::buffer::store::OutputBufferStore::new()));
        let t = super::buffer::status::BufStatusTool::new(store);
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }
}
