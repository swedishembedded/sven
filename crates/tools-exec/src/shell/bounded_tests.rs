// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `shell` tool's output contract: bounded capture, bounded results, the
//! footer, the buffer behind it, and ending a command's whole process group.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_tools_fs::{BufGrepTool, BufReadTool, OutputBufferStore};
use tokio::sync::Mutex;

use super::render::{DEFAULT_VISIBLE_BYTES, MAX_VISIBLE_BYTES};
use super::ShellTool;

fn call(args: Value) -> ToolCall {
    ToolCall {
        id: "t".into(),
        name: "shell".into(),
        args,
    }
}

fn tool() -> ShellTool {
    ShellTool {
        buffers_readable: true,
        timeout_secs: 60,
        ..ShellTool::default()
    }
}

async fn run(t: &ShellTool, command: &str) -> ToolOutput {
    t.execute(&call(json!({ "shell_command": command }))).await
}

fn handle_of(out: &ToolOutput) -> String {
    let at = out.content.find("Buffer: ").expect("a buffer handle");
    out.content[at + 8..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

#[tokio::test]
async fn a_hundred_thousand_lines_are_bounded_and_the_rest_is_retrievable() {
    let t = tool();
    let out = run(&t, "seq 1 100000").await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.len() <= DEFAULT_VISIBLE_BYTES,
        "{}",
        out.content.len()
    );
    assert!(
        out.content
            .contains("Lines: 100,000 total, 80 shown, 99,920 omitted | Bytes: 588,895 captured"),
        "{}",
        out.content
    );
    assert!(out.content.starts_with("1\n2\n") && out.content.contains("\n20\n"));
    assert!(!out.content.contains("\n21\n") && out.content.contains("\n100000\n"));
    assert!(out.content.contains("Exit: 0 | Duration: "));

    let handle = handle_of(&out);
    let read = BufReadTool::new(Arc::clone(&t.buffers));
    let omitted = read
        .execute(&call(
            json!({"handle": handle, "start_line": 50000, "end_line": 50002}),
        ))
        .await;
    assert_eq!(omitted.content, "L50000:50000\nL50001:50001\nL50002:50002");
    let grep = BufGrepTool::new(Arc::clone(&t.buffers));
    let found = grep
        .execute(&call(
            json!({"handle": handle, "pattern": "^77777$", "context_lines": 0}),
        ))
        .await;
    assert!(found.content.contains("77777"), "{}", found.content);
}

#[tokio::test]
async fn a_ten_megabyte_line_is_cut_on_a_character_and_counted_exactly() {
    // 'e' with an acute accent is two bytes; the byte cut of `head -c` ends
    // the stream inside one, which the capture must survive as well.
    let out = run(&tool(), "yes é | head -c 10000000 | tr -d '\\n'").await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.len() < DEFAULT_VISIBLE_BYTES / 2,
        "{}",
        out.content.len()
    );
    assert!(out
        .content
        .contains("Lines: 1 total, 1 shown, 0 omitted (1 cut at 2,048 bytes)"));
    assert!(
        out.content.contains("Bytes: 6,666,667 captured"),
        "{}",
        out.content
    );
    assert!(out.content.starts_with("éééé"));
}

#[tokio::test]
async fn stdin_is_end_of_file_at_once() {
    let started = Instant::now();
    let out = run(&tool(), "cat | wc -c; read x; echo \"read=$?\"").await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.starts_with("0\nread=1\n"), "{}", out.content);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn the_command_runs_non_interactively() {
    let out = run(
        &tool(),
        "echo $CI $GIT_TERMINAL_PROMPT $PAGER $GIT_PAGER $NO_COLOR",
    )
    .await;
    assert!(
        out.content.starts_with("1 0 cat cat 1\n"),
        "{}",
        out.content
    );
}

#[cfg(target_os = "linux")]
fn process_is_gone(pid: u32) -> bool {
    super::tests::process_is_gone(pid)
}

#[cfg(target_os = "linux")]
async fn wait_for_pid(file: &std::path::Path) -> u32 {
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(file) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the command never wrote {}", file.display());
}

#[cfg(target_os = "linux")]
async fn assert_gone(pid: u32) {
    for _ in 0..50 {
        if process_is_gone(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("process {pid} outlived its command");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_silent_command_that_times_out_loses_its_children_and_keeps_its_earlier_output() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("child.pid");
    let command = format!(
        "echo before; sleep 300 & echo $! > {}; wait",
        pid_file.display()
    );
    let out = tool()
        .execute(&call(
            json!({ "shell_command": command, "timeout_secs": 1 }),
        ))
        .await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.starts_with("before\n"), "{}", out.content);
    assert!(out.content.contains("Exit: timed out"), "{}", out.content);
    assert!(out.content.contains("timeout after 1s"), "{}", out.content);
    assert_gone(wait_for_pid(&pid_file).await).await;
}

/// The executor aborts the task running a tool call when its turn is
/// cancelled, which drops the call's future at its next await. Dropping must
/// end the shell and everything it started.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_cancelled_call_kills_the_whole_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("child.pid");
    let command = format!("sleep 300 & echo $! > {}; wait", pid_file.display());
    let shell = Arc::new(tool());
    let running = {
        let shell = Arc::clone(&shell);
        tokio::spawn(async move {
            shell
                .execute(&call(
                    json!({ "shell_command": command, "timeout_secs": 600 }),
                ))
                .await
        })
    };
    let child = wait_for_pid(&pid_file).await;
    assert!(
        !process_is_gone(child),
        "the child was running before the cancel"
    );

    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_gone(child).await;
}

#[tokio::test]
async fn stdout_and_stderr_are_drained_together_without_deadlock() {
    let command = "yes aaaaaaaaa | head -c 2000000 >&2 & yes bbbbbbbbb | head -c 2000000; wait";
    let out = tool()
        .execute(&call(
            json!({ "shell_command": command, "timeout_secs": 30 }),
        ))
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains("Bytes: 4,000,000 captured"),
        "{}",
        out.content
    );
    assert!(out.content.contains("Exit: 0"));
}

#[tokio::test]
async fn a_huge_stream_stays_within_the_capture_quota_and_is_still_drained() {
    let t = ShellTool {
        capture_quota_bytes: 100_000,
        ..tool()
    };
    let out = run(&t, "yes 0123456789 | head -c 5000000").await;
    assert!(
        !out.is_error,
        "the command was not blocked: {}",
        out.content
    );
    assert!(
        out.content
            .contains("Bytes: 5,000,000 observed, 100,000 retained"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains("Lines: 454,546 total"),
        "{}",
        out.content
    );
    let meta = t.buffers.lock().await.metadata(&handle_of(&out)).unwrap();
    assert_eq!(meta.total_bytes, 100_000);
    assert_eq!(meta.observed_bytes, 5_000_000);
}

#[tokio::test]
async fn the_exit_code_and_footer_survive_truncation() {
    let out = run(&tool(), "seq 1 5000; exit 3").await;
    assert!(out.is_error);
    let (_, footer) = out.content.rsplit_once("\n\n").expect("a footer block");
    assert!(
        footer.starts_with("[output truncated]\nLines: 5,000 total"),
        "{footer}"
    );
    assert!(footer.contains("Exit: 3 | Duration: "), "{footer}");
    assert!(
        footer.contains("task(action=\"grep\", handle=\"buf_"),
        "{footer}"
    );
}

#[tokio::test]
async fn short_output_carries_no_buffer_advice() {
    let out = run(&tool(), "echo hi").await;
    assert!(
        out.content.starts_with("hi\n\nExit: 0 | Duration: "),
        "{}",
        out.content
    );
    assert!(!out.content.contains("Buffer") && !out.content.contains("truncated"));
}

#[tokio::test]
async fn a_session_without_a_buffer_reader_is_not_told_to_use_one() {
    let t = ShellTool {
        buffers_readable: false,
        ..tool()
    };
    let out = run(&t, "seq 1 5000").await;
    assert!(out.content.contains("Lines: 5,000 total") && out.content.contains("Exit: 0"));
    assert!(!out.content.contains("buf_") && !out.content.contains("Use "));
}

#[tokio::test]
async fn buffers_are_not_shared_between_sessions() {
    let (a, b) = (tool(), tool());
    assert!(
        !Arc::ptr_eq(&a.buffers, &b.buffers),
        "each tool owns its store"
    );
    let handle = handle_of(&run(&a, "seq 1 500").await);

    let own = BufReadTool::new(Arc::clone(&a.buffers))
        .execute(&call(
            json!({"handle": handle, "start_line": 1, "end_line": 1}),
        ))
        .await;
    assert_eq!(own.content, "L1:1");
    let other = BufReadTool::new(Arc::clone(&b.buffers))
        .execute(&call(
            json!({"handle": handle, "start_line": 1, "end_line": 1}),
        ))
        .await;
    assert!(other.is_error && other.content.contains("unknown buffer handle"));
    let none: Arc<Mutex<OutputBufferStore>> = Arc::clone(&b.buffers);
    assert!(none.lock().await.list_metadata().is_empty());
}

#[tokio::test]
async fn max_lines_and_output_mode_choose_what_is_shown() {
    let t = tool();
    let tail = t
        .execute(&call(
            json!({"shell_command": "seq 1 500", "max_lines": 5, "output_mode": "tail"}),
        ))
        .await;
    assert!(tail
        .content
        .starts_with("[... 495 lines omitted ...]\n496\n497\n498\n499\n500\n"));
    let head = t
        .execute(&call(
            json!({"shell_command": "seq 1 500", "max_lines": 3, "output_mode": "head"}),
        ))
        .await;
    assert!(head
        .content
        .starts_with("1\n2\n3\n[... 497 lines omitted ...]\n"));
    let all = t
        .execute(&call(
            json!({"shell_command": "seq 1 5000", "max_lines": 1000000}),
        ))
        .await;
    assert!(
        all.content.contains("1,000 shown, 4,000 omitted"),
        "{}",
        all.content
    );
    assert!(all.content.len() <= MAX_VISIBLE_BYTES);
    let bad = t
        .execute(&call(
            json!({"shell_command": "true", "output_mode": "middle"}),
        ))
        .await;
    assert!(
        bad.is_error && bad.content.contains("head_tail"),
        "{}",
        bad.content
    );
}

#[test]
fn the_schema_offers_max_lines_and_output_mode_but_no_description() {
    let schema = ShellTool::default().parameters_schema();
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("max_lines") && props.contains_key("output_mode"));
    assert!(!props.contains_key("description"));
    assert_eq!(schema["required"], json!(["shell_command"]));
}
