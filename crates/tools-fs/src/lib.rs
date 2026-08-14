// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Filesystem tools: direct file read/write/edit/delete/find, and the
//! streaming subprocess-output buffer tools that let the model page through
//! large command output without loading it all into the context window.
//!
//! Split out of `sven-tools`'s `builtin/file/` + `builtin/buffer/` (5.2 of
//! the refactor plan's god-crate splits). The two subtrees were verified to
//! have zero cross-refs into each other or into any other `builtin/`
//! subdirectory; `buffer/mod.rs`'s doc comment claiming buffers are "created
//! by the task and (future) shell tools" is aspirational, not a real
//! dependency -- neither `shell/` nor `terminal/` reference
//! `OutputBufferStore` today. `GrepMatch`, shared with `sven-tools-ctx`'s
//! `context/store.rs`, moved to `sven-tool-api` (kernel tier) ahead of this
//! split rather than living in either domain crate.
//!
//! `read_image` joined later, once it was the only file left in
//! `sven-tools`'s `builtin/` root: it reads a file and produces tool output
//! about its content, the same shape as `ReadFileTool`, and already depended
//! on `sven-image` (the same crate `read_file.rs`'s inline image detection
//! uses).
pub mod buffer;
pub mod file;
pub mod read_image;

pub use buffer::{BufGrepTool, BufReadTool, BufStatusTool, BufferSource, BufferStatus, OutputBufferStore};
pub use file::{
    classify_attachment, load_attachment, AttachError, AttachFileTool, AttachOptions,
    AttachmentKind, DeleteFileTool, EditFileTool, FindFileTool, LoadedAttachment, ReadFileTool,
    WriteTool,
};
pub use read_image::ReadImageTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Moved from sven-tools's builtin/mod.rs::output_category_tests along with
// these tools themselves -- see that module's comment for why this contract
// is pinned per-tool at compile time.
#[cfg(test)]
mod output_category_tests {
    use std::sync::Arc;
    use sven_tool_api::tool::{OutputCategory, Tool};
    use tokio::sync::Mutex;

    #[test]
    fn read_file_is_filecontent() {
        let t = super::file::read_file::ReadFileTool;
        assert_eq!(t.output_category(), OutputCategory::FileContent);
    }

    #[test]
    fn write_tool_is_generic() {
        let t = super::file::write_file::WriteTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn edit_file_is_generic() {
        let t = super::file::edit_file::EditFileTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn delete_file_is_generic() {
        let t = super::file::delete_file::DeleteFileTool;
        assert_eq!(t.output_category(), OutputCategory::Generic);
    }

    #[test]
    fn find_file_tool_is_generic() {
        let t = super::file::find_file::FindFileTool;
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
