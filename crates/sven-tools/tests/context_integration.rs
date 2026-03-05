// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Integration tests for the RLM memory-mapped context tools.
//!
//! These tests exercise the full tool stack against **real source files** from
//! the sven codebase itself — the same files that are always present during
//! `cargo test`.  Every assertion is derived from the known, static content of
//! those files so that tests are self-validating without external fixtures.
//!
//! Test groups:
//!   1. `ContextStore` — low-level store operations
//!   2. `context_open` tool — mmap + metadata
//!   3. `context_read` tool — random-access line reads
//!   4. `context_grep` tool — regex search
//!   5. Workflow chains — realistic multi-step patterns
//!   6. Error handling — invalid input and boundary conditions

use std::io::Write;
use std::sync::Arc;

use serde_json::json;
use tempfile::{NamedTempFile, TempDir};
use tokio::sync::Mutex;

use sven_tools::builtin::context::store::ContextStore;
use sven_tools::builtin::context::{ContextGrepTool, ContextOpenTool, ContextReadTool};
use sven_tools::tool::{Tool, ToolCall};

// ─── Paths to always-present source files ─────────────────────────────────────

/// The store.rs file: large (~1000 lines), contains well-known symbols.
const STORE_RS: &str = "crates/sven-tools/src/builtin/context/store.rs";

/// The context module directory: 6 .rs files with known content.
const CONTEXT_DIR: &str = "crates/sven-tools/src/builtin/context";

/// The mod.rs file in the context module: small, predictable content.
const MOD_RS: &str = "crates/sven-tools/src/builtin/context/mod.rs";

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn make_store() -> Arc<Mutex<ContextStore>> {
    Arc::new(Mutex::new(ContextStore::new()))
}

fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "test".into(),
        name: name.into(),
        args,
    }
}

/// Write `content` to a `NamedTempFile` and return it.
/// The file is kept alive for the duration of the caller's scope.
fn write_tmp(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

// ─── 1. ContextStore — low-level tests ───────────────────────────────────────

mod store_tests {
    use super::*;
    use memmap2::Mmap;
    use sven_tools::builtin::context::store::build_line_index;

    fn mmap_from_bytes(content: &[u8]) -> (NamedTempFile, Mmap) {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();
        let file = std::fs::File::open(f.path()).unwrap();
        let mmap = unsafe { Mmap::map(&file).unwrap() };
        (f, mmap)
    }

    // ── Line index accuracy ───────────────────────────────────────────────────

    #[test]
    fn line_index_single_line_no_trailing_newline() {
        let (_f, mmap) = mmap_from_bytes(b"hello");
        let idx = build_line_index(&mmap);
        // No newline → only one line start at byte 0.
        assert_eq!(idx, vec![0]);
    }

    #[test]
    fn line_index_single_line_with_newline() {
        let (_f, mmap) = mmap_from_bytes(b"hello\n");
        let idx = build_line_index(&mmap);
        // Newline at byte 5, but byte 6 is EOF so no new entry.
        assert_eq!(idx, vec![0]);
    }

    #[test]
    fn line_index_three_lines_correct_offsets() {
        // "abc\nde\nfghi\n" → starts at 0, 4, 7
        let (_f, mmap) = mmap_from_bytes(b"abc\nde\nfghi\n");
        let idx = build_line_index(&mmap);
        assert_eq!(idx, vec![0, 4, 7]);
    }

    #[test]
    fn line_index_empty_lines_are_indexed() {
        // "\n\nhello\n" → 3 lines, starts at 0, 1, 2
        let (_f, mmap) = mmap_from_bytes(b"\n\nhello\n");
        let idx = build_line_index(&mmap);
        assert_eq!(idx, vec![0, 1, 2]);
    }

    #[test]
    fn line_index_matches_rust_lines_count_on_real_file() {
        // Ground truth: store.rs is 1007 lines.
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return; // Guard: runs only when the repo is present.
        }
        let content = std::fs::read(path).unwrap();
        let expected_lines = String::from_utf8_lossy(&content).lines().count();

        let file = std::fs::File::open(path).unwrap();
        let mmap = unsafe { Mmap::map(&file).unwrap() };
        let idx = build_line_index(&mmap);

        // The line index length should equal the number of lines in the file.
        // (files ending with \n: lines().count() == build_line_index().len())
        assert_eq!(
            idx.len(),
            expected_lines,
            "line index length {idx_len} != lines().count() {expected_lines}",
            idx_len = idx.len()
        );
    }

    // ── open_file metadata ────────────────────────────────────────────────────

    #[test]
    fn open_file_total_lines_matches_wc() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let expected = String::from_utf8_lossy(&std::fs::read(path).unwrap())
            .lines()
            .count();

        let mut store = ContextStore::new();
        let meta = store.open_file(path).unwrap();
        assert_eq!(meta.total_lines, expected, "open_file total_lines mismatch");
    }

    #[test]
    fn open_file_total_bytes_matches_fs_metadata() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let expected = std::fs::metadata(path).unwrap().len() as usize;
        let mut store = ContextStore::new();
        let meta = store.open_file(path).unwrap();
        assert_eq!(meta.total_bytes, expected, "open_file total_bytes mismatch");
    }

    #[test]
    fn open_file_handle_id_format() {
        let tmp = write_tmp("x\n");
        let mut store = ContextStore::new();
        let meta = store.open_file(tmp.path()).unwrap();
        assert!(
            meta.handle_id.starts_with("ctx_"),
            "handle must start with ctx_: {}",
            meta.handle_id
        );
    }

    #[test]
    fn multiple_open_files_get_unique_handles() {
        let a = write_tmp("file a\n");
        let b = write_tmp("file b\n");
        let mut store = ContextStore::new();
        let ma = store.open_file(a.path()).unwrap();
        let mb = store.open_file(b.path()).unwrap();
        assert_ne!(ma.handle_id, mb.handle_id, "handles must be unique");
    }

    // ── read_range correctness ────────────────────────────────────────────────

    #[test]
    fn read_range_single_line() {
        let tmp = write_tmp("alpha\nbeta\ngamma\n");
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();
        let text = store.read_range(&m.handle_id, 2, 2, None).unwrap();
        assert_eq!(text.trim(), "L2:beta");
    }

    #[test]
    fn read_range_first_and_last_lines() {
        let content = "FIRST LINE\nmiddle\nLAST LINE\n";
        let tmp = write_tmp(content);
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();

        let first = store.read_range(&m.handle_id, 1, 1, None).unwrap();
        assert!(first.contains("FIRST LINE"), "first line: {first}");

        let last = store.read_range(&m.handle_id, 3, 3, None).unwrap();
        assert!(last.contains("LAST LINE"), "last line: {last}");
    }

    #[test]
    fn read_range_preserves_code_content_exactly() {
        // A real Rust function definition that must survive the round-trip.
        let content = "pub fn hello() {\n    println!(\"hi\");\n}\n";
        let tmp = write_tmp(content);
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();
        let text = store.read_range(&m.handle_id, 1, 3, None).unwrap();
        assert!(text.contains("pub fn hello()"), "signature missing: {text}");
        assert!(text.contains("println!(\"hi\")"), "body missing: {text}");
        assert!(text.contains("L1:"), "line number prefix missing: {text}");
        assert!(text.contains("L2:"), "line number prefix missing: {text}");
        assert!(text.contains("L3:"), "line number prefix missing: {text}");
    }

    #[test]
    fn read_range_on_real_file_returns_known_symbol() {
        // store.rs line 88 starts `pub struct ContextStore`.
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let mut store = ContextStore::new();
        let m = store.open_file(path).unwrap();
        // Read a window around line 88; ContextStore definition must appear.
        let text = store.read_range(&m.handle_id, 85, 95, None).unwrap();
        assert!(
            text.contains("ContextStore"),
            "expected ContextStore definition in lines 85-95: {text}"
        );
    }

    #[test]
    fn read_range_start_beyond_end_is_error() {
        let tmp = write_tmp("one\ntwo\n");
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();
        let result = store.read_range(&m.handle_id, 50, 60, None);
        assert!(result.is_err(), "expected error for out-of-bounds range");
    }

    // ── grep correctness ──────────────────────────────────────────────────────

    #[test]
    fn grep_finds_all_pub_fn_in_real_file() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let mut store = ContextStore::new();
        let m = store.open_file(path).unwrap();
        let matches = store
            .grep(&m.handle_id, r"^    pub fn ", None, 0, 100)
            .unwrap();
        // store.rs has 9 pub fn methods (new, next_id, open_file, open_directory,
        // register_results, read_range, grep, chunks, read_all, metadata, contains = 11).
        assert!(
            matches.len() >= 8,
            "expected at least 8 pub fn methods in store.rs, got {}",
            matches.len()
        );
    }

    #[test]
    fn grep_match_line_number_is_accurate() {
        // "pub struct ContextStore" is at line 88 in store.rs.
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let mut store = ContextStore::new();
        let m = store.open_file(path).unwrap();
        let matches = store
            .grep(&m.handle_id, "pub struct ContextStore", None, 0, 10)
            .unwrap();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one ContextStore definition"
        );
        assert_eq!(
            matches[0].line_number, 88,
            "ContextStore should be at line 88, got {}",
            matches[0].line_number
        );
        assert!(
            matches[0].line.contains("pub struct ContextStore"),
            "match line content wrong: {}",
            matches[0].line
        );
    }

    #[test]
    fn grep_limit_caps_results() {
        // store.rs has many lines; "let" appears hundreds of times.
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let mut store = ContextStore::new();
        let m = store.open_file(path).unwrap();
        let matches = store.grep(&m.handle_id, r"\blet\b", None, 0, 5).unwrap();
        assert_eq!(
            matches.len(),
            5,
            "grep limit=5 must return exactly 5 matches"
        );
    }

    #[test]
    fn grep_context_lines_returned_around_match() {
        // The line `pub struct ContextStore {` at line 88 in store.rs has
        // known content on the lines immediately before and after it.
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let mut store = ContextStore::new();
        let m = store.open_file(path).unwrap();
        let matches = store
            .grep(&m.handle_id, "pub struct ContextStore", None, 1, 10)
            .unwrap();
        assert_eq!(matches.len(), 1);
        let m0 = &matches[0];
        assert_eq!(
            m0.context_before.len(),
            1,
            "context_lines=1: must have 1 before-line"
        );
        assert_eq!(
            m0.context_after.len(),
            1,
            "context_lines=1: must have 1 after-line"
        );
    }

    #[test]
    fn grep_no_match_returns_empty_vec() {
        let tmp = write_tmp("fn alpha() {}\n");
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();
        let matches = store
            .grep(&m.handle_id, "XYZZY_NOT_IN_FILE", None, 0, 50)
            .unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn grep_invalid_regex_returns_error() {
        let tmp = write_tmp("hello\n");
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();
        let result = store.grep(&m.handle_id, "[unclosed bracket", None, 0, 50);
        assert!(result.is_err(), "invalid regex must be an error");
        assert!(
            result.unwrap_err().contains("invalid regex"),
            "error message must mention invalid regex"
        );
    }

    // ── open_directory ────────────────────────────────────────────────────────

    #[test]
    fn open_directory_counts_all_rs_files() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }
        // There are exactly 6 .rs files in the context directory.
        let mut store = ContextStore::new();
        let meta = store.open_directory(dir, Some("*.rs"), false).unwrap();
        assert_eq!(
            meta.file_count, 6,
            "context/ should contain exactly 6 .rs files, got {}",
            meta.file_count
        );
    }

    #[test]
    fn open_directory_total_lines_matches_sum_of_files() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }
        // Compute expected total by summing each file's line count independently.
        let expected: usize = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x == "rs")
                    .unwrap_or(false)
            })
            .map(|e| {
                let c = std::fs::read_to_string(e.path()).unwrap_or_default();
                c.lines().count()
            })
            .sum();

        let mut store = ContextStore::new();
        let meta = store.open_directory(dir, Some("*.rs"), false).unwrap();
        assert_eq!(
            meta.total_lines,
            expected,
            "directory total_lines {got} != sum of file lines {expected}",
            got = meta.total_lines
        );
    }

    #[test]
    fn open_directory_grep_finds_symbol_across_files() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }
        let mut store = ContextStore::new();
        let m = store.open_directory(dir, Some("*.rs"), false).unwrap();
        // "ContextStore" appears in multiple files in the context/ directory.
        let matches = store
            .grep(&m.handle_id, "ContextStore", None, 0, 100)
            .unwrap();
        assert!(
            matches.len() >= 5,
            "expected ContextStore to appear in at least 5 places across context/, got {}",
            matches.len()
        );
        // Matches should come from multiple different files.
        let unique_files: std::collections::HashSet<_> =
            matches.iter().map(|m| m.file.clone()).collect();
        assert!(
            unique_files.len() >= 3,
            "expected matches in at least 3 files, got {} unique files",
            unique_files.len()
        );
    }

    #[test]
    fn open_directory_with_file_hint_reads_specific_file() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }
        let mut store = ContextStore::new();
        let m = store.open_directory(dir, Some("*.rs"), false).unwrap();
        // Read lines 1-5 of mod.rs via the directory handle.
        let text = store
            .read_range(&m.handle_id, 1, 5, Some("mod.rs"))
            .unwrap();
        assert!(
            text.contains("L1:"),
            "should return line-numbered content: {text}"
        );
        // mod.rs starts with the copyright comment.
        assert!(
            text.contains("Copyright") || text.contains("SPDX"),
            "mod.rs header missing in: {text}"
        );
    }

    #[test]
    fn wildcard_pattern_filters_correctly() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Readme\n").unwrap();
        std::fs::write(dir.path().join("config.toml"), "[section]\n").unwrap();

        let mut store = ContextStore::new();
        // Only *.rs should be included.
        let meta = store
            .open_directory(dir.path(), Some("*.rs"), false)
            .unwrap();
        assert_eq!(
            meta.file_count, 2,
            "*.rs pattern should include only 2 files, got {}",
            meta.file_count
        );
    }

    #[test]
    fn binary_files_skipped_in_directory() {
        let dir = TempDir::new().unwrap();
        // A text file and a binary file (contains null byte).
        std::fs::write(dir.path().join("text.txt"), "hello world\n").unwrap();
        let mut binary = vec![0x7f, 0x45, 0x4c, 0x46, 0x00, 0x01]; // ELF magic + null
        binary.extend_from_slice(b" binary content");
        std::fs::write(dir.path().join("prog.elf"), &binary).unwrap();

        let mut store = ContextStore::new();
        let meta = store.open_directory(dir.path(), None, false).unwrap();
        assert_eq!(
            meta.file_count, 1,
            "binary file must be skipped; expected 1 text file, got {}",
            meta.file_count
        );
        assert!(
            meta.summary.contains("skipped binary") || meta.summary.contains("1 skipped"),
            "summary should mention skipped binary: {}",
            meta.summary
        );
    }

    #[test]
    fn noise_directories_skipped() {
        let dir = TempDir::new().unwrap();
        // Create a .git directory and a node_modules directory.
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let nm_dir = dir.path().join("node_modules");
        std::fs::create_dir(&nm_dir).unwrap();
        std::fs::write(nm_dir.join("package.js"), "module.exports = {};\n").unwrap();

        // A real source file that should be included.
        std::fs::write(dir.path().join("index.js"), "console.log('hi');\n").unwrap();

        let mut store = ContextStore::new();
        let meta = store.open_directory(dir.path(), None, true).unwrap();
        assert_eq!(
            meta.file_count, 1,
            "only index.js should be included; .git and node_modules must be skipped"
        );
    }

    // ── chunks ────────────────────────────────────────────────────────────────

    #[test]
    fn chunks_covers_all_lines_without_overlap() {
        let content: String = (1..=25).map(|i| format!("line content {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();

        let mut seen_lines: Vec<usize> = Vec::new();
        store
            .chunks(&m.handle_id, 10, |_idx, _total, _label, text| {
                for line in text.lines() {
                    // Extract the line number from the L{n}: prefix.
                    if let Some(rest) = line.strip_prefix('L') {
                        if let Some((num_str, _)) = rest.split_once(':') {
                            if let Ok(n) = num_str.parse::<usize>() {
                                seen_lines.push(n);
                            }
                        }
                    }
                }
                Ok(())
            })
            .unwrap();

        seen_lines.sort_unstable();
        // Every line 1..=25 should appear exactly once.
        let expected: Vec<usize> = (1..=25).collect();
        assert_eq!(
            seen_lines, expected,
            "chunks must cover all lines exactly once"
        );
    }

    #[test]
    fn chunks_callback_error_stops_iteration() {
        let content: String = (1..=50).map(|i| format!("line {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();

        let mut count = 0usize;
        let result = store.chunks(&m.handle_id, 10, |idx, _total, _label, _text| {
            count += 1;
            if idx == 1 {
                Err("stop here".to_string())
            } else {
                Ok(())
            }
        });

        assert!(result.is_err(), "error from callback must propagate");
        assert_eq!(
            count, 2,
            "iteration must stop after the error callback (chunks 0 and 1)"
        );
    }

    // ── read_all ──────────────────────────────────────────────────────────────

    #[test]
    fn read_all_returns_complete_content() {
        let content = "line one\nline two\nline three\n";
        let tmp = write_tmp(content);
        let mut store = ContextStore::new();
        let m = store.open_file(tmp.path()).unwrap();
        let all = store.read_all(&m.handle_id).unwrap();
        assert!(all.contains("line one"), "read_all missing line 1: {all}");
        assert!(all.contains("line two"), "read_all missing line 2: {all}");
        assert!(all.contains("line three"), "read_all missing line 3: {all}");
    }

    // ── register_results ──────────────────────────────────────────────────────

    #[test]
    fn register_results_creates_readable_handle() {
        let mut store = ContextStore::new();
        let results_content = "=== chunk 1 of 2 ===\nfound issue at line 42\n\n\
                               === chunk 2 of 2 ===\nno issues found\n";
        let tmp = write_tmp(results_content);
        // Results must be registered by path (temp file lifetime must outlast store).
        let meta = store.register_results(tmp.path().to_path_buf(), 2).unwrap();
        assert!(
            meta.handle_id.starts_with("res_"),
            "results handle must start with res_: {}",
            meta.handle_id
        );
        let text = store.read_all(&meta.handle_id).unwrap();
        assert!(
            text.contains("found issue at line 42"),
            "results content missing: {text}"
        );
    }
}

// ─── 2. context_open tool ─────────────────────────────────────────────────────

mod open_tool_tests {
    use super::*;

    #[tokio::test]
    async fn open_real_file_output_contains_handle_and_stats() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let store = make_store();
        let tool = ContextOpenTool::new(store);
        let out = tool
            .execute(&tool_call("context_open", json!({"path": STORE_RS})))
            .await;
        assert!(!out.is_error, "context_open failed: {}", out.content);
        assert!(
            out.content.contains("handle=ctx_"),
            "output must contain handle: {}",
            out.content
        );
        assert!(
            out.content.contains("Lines:"),
            "output must contain line count: {}",
            out.content
        );
        assert!(
            out.content.contains("Bytes:"),
            "output must contain byte count: {}",
            out.content
        );
        assert!(
            out.content.contains("context_grep"),
            "output must suggest context_grep: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn open_directory_output_contains_file_count() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }
        let store = make_store();
        let tool = ContextOpenTool::new(store);
        let out = tool
            .execute(&tool_call(
                "context_open",
                json!({"path": CONTEXT_DIR, "include_pattern": "*.rs", "recursive": false}),
            ))
            .await;
        assert!(
            !out.is_error,
            "context_open directory failed: {}",
            out.content
        );
        assert!(
            out.content.contains("Files: 6"),
            "expected 6 .rs files in output: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn open_persists_handle_for_subsequent_reads() {
        // Open a file, then read it through the read tool — verifying the handle
        // is shared correctly via the Arc<Mutex<ContextStore>>.
        let tmp = write_tmp("alpha\nbeta\ngamma\n");
        let store = make_store();
        let open_tool = ContextOpenTool::new(store.clone());
        let read_tool = ContextReadTool::new(store);

        let open_out = open_tool
            .execute(&tool_call(
                "context_open",
                json!({"path": tmp.path().to_str().unwrap()}),
            ))
            .await;
        assert!(!open_out.is_error, "{}", open_out.content);

        // Extract the handle ID from the output.
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .expect("handle not found in context_open output");

        let read_out = read_tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": handle, "start_line": 2, "end_line": 2}),
            ))
            .await;
        assert!(!read_out.is_error, "{}", read_out.content);
        assert!(
            read_out.content.contains("beta"),
            "expected 'beta' on line 2: {}",
            read_out.content
        );
    }
}

// ─── 3. context_read tool ─────────────────────────────────────────────────────

mod read_tool_tests {
    use super::*;

    async fn open_path(store: &Arc<Mutex<ContextStore>>, path: &str) -> String {
        let tool = ContextOpenTool::new(store.clone());
        let out = tool
            .execute(&tool_call("context_open", json!({"path": path})))
            .await;
        assert!(!out.is_error, "open failed: {}", out.content);
        out.content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .expect("handle not found")
            .to_string()
    }

    #[tokio::test]
    async fn read_known_struct_definition_from_store_rs() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let store = make_store();
        let handle = open_path(&store, STORE_RS).await;
        let tool = ContextReadTool::new(store);

        // `pub struct ContextStore` is at line 88.
        let out = tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": handle, "start_line": 88, "end_line": 95}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("pub struct ContextStore"),
            "expected ContextStore definition: {}",
            out.content
        );
        assert!(
            out.content.contains("L88:"),
            "expected L88 line number: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn read_line_numbers_are_accurate() {
        let content: String = (1..=10).map(|i| format!("payload line {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_path(&store, tmp.path().to_str().unwrap()).await;
        let tool = ContextReadTool::new(store);

        let out = tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": handle, "start_line": 3, "end_line": 7}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Lines 3-7 should be present with correct L{n} prefixes.
        for n in 3..=7 {
            assert!(
                out.content.contains(&format!("L{}:", n)),
                "missing L{n}: prefix in: {}",
                out.content
            );
        }
        // Lines outside the range must not appear.
        for n in [1, 2, 8, 9, 10] {
            assert!(
                !out.content.contains(&format!("L{}:", n)),
                "unexpected L{n}: in output: {}",
                out.content
            );
        }
    }

    #[tokio::test]
    async fn read_utf8_content_survives_roundtrip() {
        let content = "Line with unicode: café ñoño 日本語\nSecond line: αβγδ\n";
        let tmp = write_tmp(content);
        let store = make_store();
        let handle = open_path(&store, tmp.path().to_str().unwrap()).await;
        let tool = ContextReadTool::new(store);

        let out = tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": handle, "start_line": 1, "end_line": 2}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("café"),
            "unicode content corrupted: {}",
            out.content
        );
        assert!(
            out.content.contains("αβγδ"),
            "greek letters missing: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn read_empty_range_returns_gracefully() {
        let tmp = write_tmp("a\nb\nc\n");
        let store = make_store();
        let handle = open_path(&store, tmp.path().to_str().unwrap()).await;
        let tool = ContextReadTool::new(store);

        // start_line beyond file length.
        let out = tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": handle, "start_line": 999, "end_line": 1000}),
            ))
            .await;
        assert!(
            out.is_error,
            "expected error for out-of-range: {}",
            out.content
        );
    }
}

// ─── 4. context_grep tool ─────────────────────────────────────────────────────

mod grep_tool_tests {
    use super::*;

    async fn open_path(store: &Arc<Mutex<ContextStore>>, path: &str) -> String {
        let tool = ContextOpenTool::new(store.clone());
        let out = tool
            .execute(&tool_call("context_open", json!({"path": path})))
            .await;
        assert!(!out.is_error, "open failed: {}", out.content);
        out.content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .expect("handle not found")
            .to_string()
    }

    #[tokio::test]
    async fn grep_finds_pub_struct_in_store_rs() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let store = make_store();
        let handle = open_path(&store, STORE_RS).await;
        let tool = ContextGrepTool::new(store);

        let out = tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": r"^pub struct "}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // There are 5 pub struct definitions in store.rs:
        // FileEntry, ContextKind (enum), ContextMetadata, ContextHandle, ContextStore, GrepMatch
        assert!(
            out.content.contains("match"),
            "expected match count in output: {}",
            out.content
        );
        for name in &["ContextStore", "ContextMetadata", "GrepMatch"] {
            assert!(
                out.content.contains(name),
                "expected {name} in grep output: {}",
                out.content
            );
        }
    }

    #[tokio::test]
    async fn grep_returns_line_numbers_in_output() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let store = make_store();
        let handle = open_path(&store, STORE_RS).await;
        let tool = ContextGrepTool::new(store);

        let out = tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": "pub struct ContextStore", "context_lines": 0}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Output format: "{file}:L88: pub struct ContextStore {"
        assert!(
            out.content.contains("L88"),
            "expected line 88 in output: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn grep_context_lines_shown_in_output() {
        let content = "setup line\nTARGET MATCH\nteardown line\n";
        let tmp = write_tmp(content);
        let store = make_store();
        let handle = open_path(&store, tmp.path().to_str().unwrap()).await;
        let tool = ContextGrepTool::new(store);

        let out = tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": "TARGET MATCH", "context_lines": 1}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("setup line"),
            "context before missing: {}",
            out.content
        );
        assert!(
            out.content.contains("teardown line"),
            "context after missing: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn grep_limit_respected_in_output() {
        // Generate a file with 100 lines all matching.
        let content: String = (1..=100).map(|i| format!("match_{}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_path(&store, tmp.path().to_str().unwrap()).await;
        let tool = ContextGrepTool::new(store);

        let out = tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": "match_", "limit": 7}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Should report exactly 7 matches.
        assert!(
            out.content.contains("7 match"),
            "expected '7 match(es)' in output: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn grep_across_directory_reports_file_paths() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }
        let store = make_store();
        let open_tool = ContextOpenTool::new(store.clone());
        let grep_tool = ContextGrepTool::new(store);

        let open_out = open_tool
            .execute(&tool_call(
                "context_open",
                json!({"path": CONTEXT_DIR, "include_pattern": "*.rs", "recursive": false}),
            ))
            .await;
        assert!(!open_out.is_error, "{}", open_out.content);

        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        let grep_out = grep_tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": r"^pub fn ", "limit": 50}),
            ))
            .await;
        assert!(!grep_out.is_error, "{}", grep_out.content);

        // Matches from multiple files — the output must mention at least two .rs files.
        let rs_file_refs = grep_out
            .content
            .lines()
            .filter(|l| l.contains(".rs:L"))
            .count();
        assert!(
            rs_file_refs >= 2,
            "grep across directory must report matches from multiple .rs files: {}",
            grep_out.content
        );
    }
}

// ─── 5. Workflow chains ───────────────────────────────────────────────────────

mod workflow_tests {
    use super::*;

    /// Simulate the full RLM workflow: open → grep → read.
    /// This mirrors exactly what the model does when asked to locate and
    /// inspect a function in a large file.
    #[tokio::test]
    async fn open_grep_read_workflow_on_real_codebase() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }
        let store = make_store();

        // Step 1: Open.
        let open_tool = ContextOpenTool::new(store.clone());
        let open_out = open_tool
            .execute(&tool_call("context_open", json!({"path": STORE_RS})))
            .await;
        assert!(
            !open_out.is_error,
            "step 1 open failed: {}",
            open_out.content
        );
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        // Step 2: Grep for `pub fn grep` to find the grep method entry point.
        let grep_tool = ContextGrepTool::new(store.clone());
        let grep_out = grep_tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": r"pub fn grep\b", "context_lines": 0}),
            ))
            .await;
        assert!(
            !grep_out.is_error,
            "step 2 grep failed: {}",
            grep_out.content
        );
        assert!(
            grep_out.content.contains("pub fn grep"),
            "grep must find pub fn grep: {}",
            grep_out.content
        );

        // Extract the line number from the grep output.
        let grep_line: usize = grep_out
            .content
            .lines()
            .find(|l| l.contains("pub fn grep"))
            .and_then(|l| {
                // Format is "path:L{n}: content"
                l.split(":L").nth(1)?.split(':').next()?.parse().ok()
            })
            .expect("could not extract line number from grep output");

        assert!(
            grep_line >= 390 && grep_line <= 420,
            "pub fn grep should be around line 397-410, got {grep_line}"
        );

        // Step 3: Read the function body.
        let read_tool = ContextReadTool::new(store);
        let read_out = read_tool
            .execute(&tool_call(
                "context_read",
                json!({
                    "handle": handle,
                    "start_line": grep_line,
                    "end_line": grep_line + 20
                }),
            ))
            .await;
        assert!(
            !read_out.is_error,
            "step 3 read failed: {}",
            read_out.content
        );
        // The function body must mention the regex parameter.
        assert!(
            read_out.content.contains("pattern"),
            "function body should mention 'pattern': {}",
            read_out.content
        );
    }

    /// Open a directory, grep for a symbol, read the matching file section —
    /// demonstrating cross-file analysis.
    #[tokio::test]
    async fn directory_grep_then_read_specific_file() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }
        let store = make_store();

        // Open the directory.
        let open_tool = ContextOpenTool::new(store.clone());
        let open_out = open_tool
            .execute(&tool_call(
                "context_open",
                json!({"path": CONTEXT_DIR, "include_pattern": "*.rs", "recursive": false}),
            ))
            .await;
        assert!(!open_out.is_error, "{}", open_out.content);
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        // Grep for SubQueryRunner (defined in query_runner.rs).
        let grep_tool = ContextGrepTool::new(store.clone());
        let grep_out = grep_tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": "pub trait SubQueryRunner"}),
            ))
            .await;
        assert!(!grep_out.is_error, "{}", grep_out.content);
        assert!(
            grep_out.content.contains("SubQueryRunner"),
            "SubQueryRunner definition not found: {}",
            grep_out.content
        );
        assert!(
            grep_out.content.contains("query_runner.rs"),
            "match should be in query_runner.rs: {}",
            grep_out.content
        );

        // Read query_runner.rs lines 1-32 via the directory handle.
        let read_tool = ContextReadTool::new(store);
        let read_out = read_tool
            .execute(&tool_call(
                "context_read",
                json!({
                    "handle": handle,
                    "start_line": 1,
                    "end_line": 32,
                    "file": "query_runner.rs"
                }),
            ))
            .await;
        assert!(!read_out.is_error, "{}", read_out.content);
        assert!(
            read_out.content.contains("SubQueryRunner"),
            "trait definition must be in first 32 lines: {}",
            read_out.content
        );
    }

    /// Register results and read them back — simulating the context_query
    /// results-handle workflow without invoking the LLM.
    #[tokio::test]
    async fn results_handle_readable_after_registration() {
        let mut store_inner = ContextStore::new();

        let results = r#"=== chunk 1 of 3 ===
MISRA Rule 11.3 violation at line 42: cast from integer to pointer.

=== chunk 2 of 3 ===
No issues found.

=== chunk 3 of 3 ===
MISRA Rule 14.4 violation at line 87: non-boolean condition in if statement.
"#;
        let tmp = write_tmp(results);
        let meta = store_inner
            .register_results(tmp.path().to_path_buf(), 3)
            .unwrap();
        assert!(meta.handle_id.starts_with("res_"));
        assert_eq!(meta.file_count, 1);

        // Read back specific chunks.
        let text = store_inner.read_all(&meta.handle_id).unwrap();
        assert!(text.contains("Rule 11.3"), "chunk 1 content missing");
        assert!(text.contains("No issues found"), "chunk 2 content missing");
        assert!(text.contains("Rule 14.4"), "chunk 3 content missing");

        // Grep the results for violations.
        let violations = store_inner
            .grep(&meta.handle_id, "violation", None, 0, 10)
            .unwrap();
        assert_eq!(violations.len(), 2, "expected exactly 2 violation lines");
        assert!(violations[0].line_number < violations[1].line_number);
    }
}

// ─── 6. Error handling ────────────────────────────────────────────────────────

mod error_tests {
    use super::*;

    #[tokio::test]
    async fn open_nonexistent_file_is_error() {
        let store = make_store();
        let tool = ContextOpenTool::new(store);
        let out = tool
            .execute(&tool_call(
                "context_open",
                json!({"path": "/tmp/sven_no_such_file_xyzzy_12345.rs"}),
            ))
            .await;
        assert!(out.is_error, "expected error for nonexistent file");
        assert!(
            out.content.contains("context_open failed"),
            "error message format wrong: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn read_unknown_handle_is_error() {
        let store = make_store();
        let tool = ContextReadTool::new(store);
        let out = tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": "ctx_dead", "start_line": 1, "end_line": 5}),
            ))
            .await;
        assert!(out.is_error);
        assert!(
            out.content.contains("unknown handle"),
            "error must mention unknown handle: {}",
            out.content
        );
        assert!(
            out.content.contains("context_open"),
            "error must suggest context_open: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn grep_unknown_handle_is_error() {
        let store = make_store();
        let tool = ContextGrepTool::new(store);
        let out = tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": "ctx_dead", "pattern": "anything"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown handle"));
    }

    #[tokio::test]
    async fn read_end_before_start_is_error() {
        let tmp = write_tmp("a\nb\nc\n");
        let store = make_store();
        let open_tool = ContextOpenTool::new(store.clone());
        let open_out = open_tool
            .execute(&tool_call(
                "context_open",
                json!({"path": tmp.path().to_str().unwrap()}),
            ))
            .await;
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        let read_tool = ContextReadTool::new(store);
        let out = read_tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": handle, "start_line": 5, "end_line": 2}),
            ))
            .await;
        assert!(out.is_error, "end < start must be an error");
        assert!(
            out.content.contains("end_line") || out.content.contains("start_line"),
            "error must mention the offending parameters: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn open_missing_path_param_is_error() {
        let store = make_store();
        let tool = ContextOpenTool::new(store);
        let out = tool.execute(&tool_call("context_open", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[tokio::test]
    async fn read_zero_start_line_is_error() {
        let tmp = write_tmp("x\n");
        let store = make_store();
        let open_tool = ContextOpenTool::new(store.clone());
        let open_out = open_tool
            .execute(&tool_call(
                "context_open",
                json!({"path": tmp.path().to_str().unwrap()}),
            ))
            .await;
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        let read_tool = ContextReadTool::new(store);
        let out = read_tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": handle, "start_line": 0, "end_line": 1}),
            ))
            .await;
        assert!(out.is_error, "start_line=0 must be an error");
    }

    #[tokio::test]
    async fn grep_invalid_regex_is_error() {
        let tmp = write_tmp("test\n");
        let store = make_store();
        let open_tool = ContextOpenTool::new(store.clone());
        let open_out = open_tool
            .execute(&tool_call(
                "context_open",
                json!({"path": tmp.path().to_str().unwrap()}),
            ))
            .await;
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        let grep_tool = ContextGrepTool::new(store);
        let out = grep_tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": "[invalid"}),
            ))
            .await;
        assert!(out.is_error, "invalid regex must produce an error");
    }
}
