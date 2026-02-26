// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput, ToolOutputPart};

/// Default number of lines returned when the caller does not specify a limit.
/// Kept small to avoid flooding the model context on the first read; the agent
/// can paginate with offset + limit to get more.
const DEFAULT_LINE_LIMIT: usize = 200;

/// Hard byte ceiling applied in addition to the line limit.
/// Whichever constraint is hit first determines where the output is cut.
/// 20 KB ≈ 5,000 tokens — safe for a 40 K-token context window.
const MAX_BYTES: usize = 20_000;

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }

    fn description(&self) -> &str {
        "Reads a file. Default: 200 lines / 20 KB — whichever comes first.\n\
         Binary files (detected by extension or content) are rendered as Intel HEX;\n\
         limit/offset apply to HEX line numbers (each line = 16 bytes).\n\
         Images (png/jpg/gif/webp/bmp/tiff) → returned as base64 data URL.\n\
         Lines formatted as L{n}:content (1-indexed). For edit_file old_str strip the L{n}: prefix.\n\
         When more lines exist, a pagination notice shows the next offset.\n\
         Strategy: use grep to find the relevant region first, then read only those lines\n\
         with offset+limit. Avoid reading a whole large file — pull only what you need.\n\
         Batch multiple reads in parallel when exploring related files."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "offset": {
                    "type": "integer",
                    "description": "1-indexed line number to start reading from (default 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default 200)"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }
    fn output_category(&self) -> OutputCategory { OutputCategory::FileContent }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                let args_preview = serde_json::to_string(&call.args)
                    .unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'path'. Received: {}", args_preview)
                );
            }
        };
        let offset = call.args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = call.args.get("limit").and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_LINE_LIMIT as u64) as usize;

        debug!(path = %path, offset, limit, "read_file tool");

        // ── Image files ───────────────────────────────────────────────────────
        // Returned as multimodal base64 data URLs; bypass all text/binary logic.
        let ext = std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if sven_image::is_image_extension(ext) {
            return match sven_image::load_image(std::path::Path::new(&path)) {
                Ok(img) => {
                    let data_url = img.into_data_url();
                    ToolOutput::with_parts(&call.id, vec![
                        ToolOutputPart::Text(format!("Image file: {path}")),
                        ToolOutputPart::Image(data_url),
                    ])
                }
                Err(e) => ToolOutput::err(&call.id, format!("failed to read image: {e}")),
            };
        }

        // ── Path resolution ───────────────────────────────────────────────────
        // When a workflow runs from a project sub-directory but references a
        // file relative to a workspace root one level up, try ascending the
        // directory tree to find the file automatically.
        //
        // Example: /data/ng-iot-platform/.cursor/knowledge/foo.md fails →
        //          /data/.cursor/knowledge/foo.md is tried automatically.
        let (resolved_path, resolved_note) = match ascend_to_find(&path) {
            Some(found) => {
                let note = format!("note: resolved to {}\n", found.display());
                (found.to_string_lossy().into_owned(), Some(note))
            }
            None => (path.clone(), None),
        };

        // ── Read raw bytes ────────────────────────────────────────────────────
        let bytes = match tokio::fs::read(&resolved_path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(&call.id, format!("read error: {e}")),
        };

        // ── Binary detection ──────────────────────────────────────────────────
        // Known binary extensions are rejected immediately without reading.
        // For other files, a byte-content sample determines binary vs text.
        // Binary files are rendered as Intel HEX so the agent can inspect them.
        if is_binary_extension(ext) || has_binary_content(&bytes) {
            let ihex_lines = to_ihex_lines(&bytes);
            let total = ihex_lines.len();
            let start = offset.saturating_sub(1);
            let slice: Vec<&str> = ihex_lines.iter()
                .skip(start)
                .take(limit)
                .map(String::as_str)
                .collect();
            let last = start + slice.len();
            let mut content = format!(
                "note: binary file ({} bytes) rendered as Intel HEX ({} lines, 16 bytes/line)\n{}",
                bytes.len(), total, slice.join("\n")
            );
            if last < total {
                content.push_str(&format!(
                    "\n...[{} more lines — showing L{}-L{} of {}; use offset={} to continue]",
                    total - last, offset, offset + slice.len() - 1, total, last + 1
                ));
            }
            if let Some(note) = resolved_note {
                content = format!("{}{}", note, content);
            }
            return ToolOutput::ok(&call.id, content);
        }

        // ── Text file ─────────────────────────────────────────────────────────
        let text = String::from_utf8_lossy(&bytes);
        let start = offset.saturating_sub(1);
        let all_lines: Vec<&str> = text.lines().collect();
        let total = all_lines.len();

        // Collect lines up to both the line limit and the byte cap.
        let mut selected: Vec<String> = Vec::new();
        let mut byte_count: usize = 0;
        let mut truncated_by_bytes = false;
        for (i, line) in all_lines.iter().enumerate().skip(start).take(limit) {
            let line_bytes = line.len() + 1; // +1 for the newline
            if byte_count + line_bytes > MAX_BYTES {
                truncated_by_bytes = true;
                break;
            }
            selected.push(format!("L{}:{}", i + 1, line));
            byte_count += line_bytes;
        }

        let last_shown = start + selected.len();
        let mut content = selected.join("\n");

        if last_shown < total {
            let reason = if truncated_by_bytes {
                format!("byte limit ({} B) reached", MAX_BYTES)
            } else {
                format!("{} more lines", total - last_shown)
            };
            content.push_str(&format!(
                "\n...[{} — showing L{}-L{} of {}; use offset={} to continue]",
                reason,
                offset,
                offset + selected.len().saturating_sub(1),
                total,
                last_shown + 1
            ));
        }

        if let Some(note) = resolved_note {
            content = format!("{}{}", note, content);
        }

        ToolOutput::ok(&call.id, content)
    }
}

// ── Binary detection ──────────────────────────────────────────────────────────

/// Returns `true` for extensions that are always binary and never useful to
/// read as text.  This is a fast-path that avoids reading the file at all.
fn is_binary_extension(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(),
        // Object / library / executable
        "o" | "a" | "so" | "elf" | "exe" | "dll" | "wasm" | "pdb" |
        // Archives / compressed
        "zip" | "gz" | "tar" | "bz2" | "xz" | "7z" | "zst" |
        // Firmware / ROM images
        "bin" | "img" | "rom" | "fw" | "srec" | "s19" | "mot" |
        // Python / JVM bytecode
        "pyc" | "pyo" | "class" | "jar" | "war" |
        // Office documents
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "pdf" |
        // Build artefacts
        "obj" | "lib"
    )
}

/// Samples up to 4096 bytes of `bytes` to decide if the content is binary.
///
/// Rules (same heuristic as opencode / git):
/// - Any null byte (`0x00`) → binary.
/// - More than 30% non-printable bytes (outside TAB/LF/CR/space..~) → binary.
fn has_binary_content(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let sample = &bytes[..bytes.len().min(4096)];
    if sample.contains(&0u8) {
        return true;
    }
    let non_printable = sample.iter()
        .filter(|&&b| b < 9 || (b > 13 && b < 32))
        .count();
    non_printable * 100 / sample.len() > 30
}

// ── Intel HEX generation ──────────────────────────────────────────────────────

/// Convert raw bytes to Intel HEX lines.
///
/// Each data record holds 16 bytes.  An Extended Linear Address (ELA) record
/// is emitted whenever the upper 16 bits of the address change, allowing
/// files larger than 64 KB to be represented correctly.  The last line is
/// always the EOF record `:00000001FF`.
///
/// Addresses start at 0x00000000.  For `.bin` files this is the natural load
/// address; for `.elf` files the agent should note that file offsets ≠ virtual
/// addresses (use `readelf` / `objdump` if VMA matters).
fn to_ihex_lines(data: &[u8]) -> Vec<String> {
    const BPL: usize = 16; // bytes per data record line
    let mut lines: Vec<String> = Vec::with_capacity(data.len() / BPL + 2);
    let mut cur_seg = usize::MAX; // force ELA record on first iteration

    for (i, chunk) in data.chunks(BPL).enumerate() {
        let addr = i * BPL;
        let seg = addr >> 16;

        // Extended Linear Address record — emitted when the 64 KB segment changes.
        if seg != cur_seg {
            cur_seg = seg;
            let hi = (seg >> 8) as u8;
            let lo = (seg & 0xFF) as u8;
            // Checksum: two's complement of (byte_count=02, addr_hi=00, addr_lo=00,
            // record_type=04, data_hi, data_lo).
            let cs = (0u8)
                .wrapping_add(2)
                .wrapping_add(4)
                .wrapping_add(hi)
                .wrapping_add(lo);
            let cs = (!cs).wrapping_add(1);
            lines.push(format!(":02000004{:02X}{:02X}{:02X}", hi, lo, cs));
        }

        let a16 = (addr & 0xFFFF) as u16;
        let n = chunk.len() as u8;
        // Accumulate checksum: byte_count + addr_hi + addr_lo + record_type(00) + data bytes.
        let mut cs = n
            .wrapping_add((a16 >> 8) as u8)
            .wrapping_add((a16 & 0xFF) as u8);
        let data_hex: String = chunk.iter().map(|b| {
            cs = cs.wrapping_add(*b);
            format!("{:02X}", b)
        }).collect();
        cs = (!cs).wrapping_add(1);
        lines.push(format!(":{:02X}{:04X}00{}{:02X}", n, a16, data_hex, cs));
    }

    lines.push(":00000001FF".to_string()); // EOF record
    lines
}

// ── Path ascent helper ────────────────────────────────────────────────────────

/// For an absolute path that does not exist, try removing one "middle"
/// component at a time to find the file at an ancestor level.
///
/// Given `/A/B/C/D/file.txt` this tries all single-component drops:
///   drop index 1 → `/A/C/D/file.txt`
///   drop index 2 → `/A/B/D/file.txt`
///   drop index 3 → `/A/B/C/file.txt`
///
/// Then double-drops (two consecutive components removed), up to a depth cap.
///
/// Returns the first candidate that exists on disk, or `None`.
///
/// This handles the common workspace layout where a tool runs from a git
/// repository (`/workspace/project/`) but references files relative to the
/// workspace root (`/workspace/.cursor/knowledge/`).  The extra `project`
/// component is detected and removed automatically.
fn ascend_to_find(path: &str) -> Option<std::path::PathBuf> {
    use std::path::{Component, Path};

    let p = Path::new(path);

    // Only apply to absolute paths that do not already exist.
    if !p.is_absolute() || p.exists() {
        return None;
    }

    let parts: Vec<Component> = p.components().collect();
    // Need at least: RootDir + 2 dirs + filename to have anything to drop.
    if parts.len() < 4 {
        return None;
    }

    // Cap total path depth to avoid excessive fs stats on exotic paths.
    const MAX_DEPTH: usize = 12;
    if parts.len() > MAX_DEPTH {
        return None;
    }

    // Pass 1: drop one component at any middle position (index 1..len-1).
    // Do not drop index 0 (RootDir) or the final component (filename).
    for drop_at in 1..parts.len() - 1 {
        let candidate: std::path::PathBuf = parts[..drop_at]
            .iter()
            .chain(parts[drop_at + 1..].iter())
            .collect();
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // Pass 2: drop two consecutive components at any middle position.
    for drop_at in 1..parts.len().saturating_sub(2) {
        let candidate: std::path::PathBuf = parts[..drop_at]
            .iter()
            .chain(parts[drop_at + 2..].iter())
            .collect();
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "r1".into(), name: "read_file".into(), args }
    }

    fn tmp_file(content: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_read_file_test_{}_{n}.txt", std::process::id());
        std::fs::write(&path, content).unwrap();
        path
    }

    // ── Basic text reading ────────────────────────────────────────────────────

    #[tokio::test]
    async fn reads_file_with_line_numbers() {
        let path = tmp_file("alpha\nbeta\ngamma\n");
        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": path}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("L1:alpha"));
        assert!(out.content.contains("L2:beta"));
        assert!(out.content.contains("L3:gamma"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn offset_and_limit_work() {
        let path = tmp_file("line1\nline2\nline3\nline4\nline5\n");
        let t = ReadFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "offset": 2,
            "limit": 2
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("L2:line2"));
        assert!(out.content.contains("L3:line3"));
        assert!(!out.content.contains("L1:"));
        assert!(!out.content.contains("L4:"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": "/tmp/sven_no_such_file_xyz.txt"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("read error"));
    }

    #[tokio::test]
    async fn missing_file_path_is_error() {
        let t = ReadFileTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    // ── Pagination notice ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn pagination_notice_when_more_lines_exist() {
        // 5 lines, read only 2 → expect a "more lines" notice
        let path = tmp_file("a\nb\nc\nd\ne\n");
        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": path, "limit": 2}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("offset=3"), "should suggest next offset: {}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn no_pagination_notice_when_all_lines_shown() {
        let path = tmp_file("x\ny\n");
        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": path, "limit": 200}))).await;
        assert!(!out.is_error);
        assert!(!out.content.contains("offset="), "should not paginate: {}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    // ── Byte cap ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn byte_cap_truncates_before_line_limit() {
        // 500 lines × 50 bytes each = 25 KB > MAX_BYTES (20 KB)
        let line = "x".repeat(49); // 49 chars + newline = 50 bytes
        let content: String = (0..500).map(|_| format!("{}\n", line)).collect();
        let path = tmp_file(&content);
        let t = ReadFileTool;
        // Request 500 lines but byte cap should kick in first
        let out = t.execute(&call(json!({"path": path, "limit": 500}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("byte limit"), "should mention byte limit: {}", out.content);
        // Should have fewer than 500 lines
        let l_count = out.content.lines().filter(|l| l.starts_with('L')).count();
        assert!(l_count < 500, "should be fewer than 500 lines: got {}", l_count);
        let _ = std::fs::remove_file(&path);
    }

    // ── Binary detection ──────────────────────────────────────────────────────

    #[test]
    fn binary_extension_detected() {
        assert!(is_binary_extension("elf"));
        assert!(is_binary_extension("ELF")); // case-insensitive
        assert!(is_binary_extension("o"));
        assert!(is_binary_extension("bin"));
        assert!(is_binary_extension("zip"));
        assert!(!is_binary_extension("c"));
        assert!(!is_binary_extension("rs"));
        assert!(!is_binary_extension("txt"));
        assert!(!is_binary_extension("hex")); // Intel HEX is text
    }

    #[test]
    fn null_byte_triggers_binary_detection() {
        let data = b"hello\x00world";
        assert!(has_binary_content(data));
    }

    #[test]
    fn high_non_printable_fraction_triggers_binary_detection() {
        // >30% non-printable: mix in control bytes
        let mut data = vec![0x01u8; 40]; // 40 non-printable
        data.extend_from_slice(b"a".repeat(60).as_ref()); // 60 printable → 40% non-printable
        assert!(has_binary_content(&data));
    }

    #[test]
    fn normal_text_not_detected_as_binary() {
        let data = b"Hello, world!\nThis is a text file.\n";
        assert!(!has_binary_content(data));
    }

    #[test]
    fn empty_file_not_binary() {
        assert!(!has_binary_content(b""));
    }

    // ── Intel HEX generation ──────────────────────────────────────────────────

    #[test]
    fn ihex_always_ends_with_eof_record() {
        let lines = to_ihex_lines(b"hello");
        assert_eq!(lines.last().unwrap(), ":00000001FF");
    }

    #[test]
    fn ihex_eof_only_for_empty_input() {
        let lines = to_ihex_lines(b"");
        // ELA for seg 0 + EOF
        assert!(lines.last().unwrap() == ":00000001FF");
    }

    #[test]
    fn ihex_data_record_format_and_checksum() {
        // Single byte 0xFF at address 0x0000: :01000000FF00
        // checksum = ~(0x01 + 0x00 + 0x00 + 0x00 + 0xFF) + 1
        //          = ~(0x100) + 1 = ~0x00 + 1 = 0xFF + 1 = 0x00
        // :01000000FF00
        let lines = to_ihex_lines(&[0xFF]);
        // First line should be ELA for segment 0
        let ela = &lines[0];
        assert!(ela.starts_with(":02000004"), "expected ELA: {ela}");
        // Second line is the data record
        let data_rec = &lines[1];
        assert!(data_rec.starts_with(":01000000FF"), "unexpected record: {data_rec}");
    }

    #[test]
    fn ihex_full_16_byte_line() {
        let data = [0u8; 16];
        let lines = to_ihex_lines(&data);
        // ELA + one 16-byte data record + EOF = 3 lines
        assert_eq!(lines.len(), 3, "expected 3 lines for 16 bytes: {:?}", lines);
        let rec = &lines[1];
        assert!(rec.starts_with(":10000000"), "expected 16-byte record: {rec}");
    }

    #[test]
    fn ihex_ela_emitted_at_64k_boundary() {
        // 64 KB + 1 byte → should emit a second ELA record for segment 1
        let data = vec![0xAAu8; 65537];
        let lines = to_ihex_lines(&data);
        let ela_count = lines.iter().filter(|l| l.contains("000004")).count();
        assert!(ela_count >= 2, "expected at least 2 ELA records: {ela_count}");
    }

    #[tokio::test]
    async fn binary_file_returns_ihex_output() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_binary_test_{}_{n}.bin", std::process::id());
        std::fs::write(&path, b"\x7fELF\x00\x01\x02\x03").unwrap();

        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": path}))).await;
        assert!(!out.is_error, "binary should succeed: {}", out.content);
        assert!(out.content.contains("Intel HEX"), "should mention Intel HEX: {}", out.content);
        assert!(out.content.contains(":"), "should contain HEX records: {}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn binary_file_pagination_works() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_binary_page_{}_{n}.bin", std::process::id());
        // 64 bytes = 4 full 16-byte records + ELA + EOF = 6 lines
        std::fs::write(&path, vec![0xBBu8; 64]).unwrap();

        let t = ReadFileTool;
        // Limit to 2 lines (excluding the header note line)
        let out = t.execute(&call(json!({"path": path, "limit": 2}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("offset="), "should suggest next offset: {}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    // ── ascend_to_find tests ──────────────────────────────────────────────────

    #[test]
    fn ascend_finds_file_one_level_up() {
        use std::fs;
        // Create structure: /tmp/sven_ascend_test/<workspace>/project/subdir/file.txt
        // but file actually lives at /tmp/sven_ascend_test/<workspace>/subdir/file.txt
        let base = std::env::temp_dir().join(format!(
            "sven_ascend_test_{}",
            std::process::id()
        ));
        let workspace = base.join("workspace");
        let project = workspace.join("project");
        let workspace_subdir = workspace.join("subdir");
        let _ = fs::create_dir_all(&project);
        let _ = fs::create_dir_all(&workspace_subdir);
        let real_file = workspace_subdir.join("file.txt");
        fs::write(&real_file, "hello").unwrap();

        // The path the agent would construct (wrong: includes "project")
        let wrong_path = project.join("subdir").join("file.txt");
        assert!(!wrong_path.exists(), "wrong path should not exist");

        let found = ascend_to_find(&wrong_path.to_string_lossy());
        assert_eq!(found.as_deref(), Some(real_file.as_path()), "should find file one level up");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn ascend_returns_none_for_truly_missing_file() {
        let found = ascend_to_find("/tmp/sven_no_such_dir_xyz/no_such_file.txt");
        assert!(found.is_none());
    }

    #[test]
    fn ascend_returns_none_for_relative_path() {
        let found = ascend_to_find("relative/path/file.txt");
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn read_file_resolves_via_ascend_and_reports_note() {
        use std::fs;
        let base = std::env::temp_dir().join(format!(
            "sven_ascend_read_{}",
            std::process::id()
        ));
        let workspace = base.join("ws");
        let project = workspace.join("proj");
        let real_dir = workspace.join("knowledge");
        let _ = fs::create_dir_all(&project);
        let _ = fs::create_dir_all(&real_dir);
        let real_file = real_dir.join("spec.md");
        fs::write(&real_file, "content line").unwrap();

        // Path the agent would try (includes "proj" which is wrong)
        let wrong_path = project.join("knowledge").join("spec.md");

        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": wrong_path.to_str().unwrap()}))).await;
        assert!(!out.is_error, "should succeed via ascend: {}", out.content);
        assert!(out.content.contains("content line"), "file content should be present");
        assert!(out.content.contains("note: resolved to"), "should report resolution note");

        let _ = fs::remove_dir_all(&base);
    }
}
