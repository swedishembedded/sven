// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Rich ratatui rendering for tool calls and results.
//!
//! Produces styled [`Line`]s from tool-call data so each tool category gets
//! visually distinct, informative rendering.  All functions receive plain data
//! (strings, JSON) and produce ratatui types; the only ratatui dependency is
//! here, not in `sven-tools`.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sven_tools::ToolDisplay;

use super::theme::{BAR_ERROR, BAR_TOOL, TEXT, TEXT_DIM};
use super::width_utils::truncate_to_width;

// ── Category colours ──────────────────────────────────────────────────────────

fn category_color(category: &str) -> Color {
    match category {
        "file" => Color::Rgb(100, 180, 255),
        "shell" => Color::Rgb(120, 220, 130),
        "search" => Color::Rgb(180, 140, 255),
        "web" => Color::Rgb(80, 200, 220),
        "system" => Color::Rgb(200, 160, 60),
        "agent" => Color::Rgb(220, 120, 180),
        _ => BAR_TOOL,
    }
}

// ── Path display helpers ──────────────────────────────────────────────────────

/// Split a path into `(directory, filename)` for dimmed-dir / bright-name styling.
fn split_path_display(path: &str) -> (String, String) {
    if let Some(slash) = path.rfind('/') {
        let dir = &path[..=slash];
        let name = &path[slash + 1..];
        (dir.to_string(), name.to_string())
    } else {
        (String::new(), path.to_string())
    }
}

// ── Collapsed one-liner ───────────────────────────────────────────────────────

/// Render a single-line collapsed view for a tool call as `Vec<Span>`.
///
/// Layout: `<icon> <display_name>  <summary>  <duration>`
///
/// Falls back to plain text spans if no registry entry is found.
pub fn render_tool_call_collapsed(
    tool_name: &str,
    args: &serde_json::Value,
    duration: Option<f32>,
    display: Option<&dyn ToolDisplay>,
    max_cols: usize,
) -> Vec<Span<'static>> {
    let (icon, label, summary, category) = if let Some(d) = display {
        (
            d.icon().to_string(),
            d.display_name().to_string(),
            d.collapsed_summary(args),
            d.category().to_string(),
        )
    } else {
        (
            sven_tools::tool_icon(tool_name).to_string(),
            tool_name.to_string(),
            sven_tools::tool_smart_summary(tool_name, args),
            sven_tools::tool_category(tool_name).to_string(),
        )
    };

    let accent = category_color(&category);
    let icon_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    let summary_style = Style::default().fg(TEXT_DIM);
    let dur_style = Style::default().fg(Color::Rgb(120, 120, 140));

    let dur_str = if let Some(d) = duration {
        format!("  {:.1}s", d)
    } else {
        String::new()
    };

    let summary_part = if summary.is_empty() {
        String::new()
    } else {
        // Reserve ~20 cols for icon + space + label + duration; use rest for summary.
        let overhead = 2 + icon.len() + 1 + label.len() + 8; // 8 for duration "  99.9s"
        format!(
            "  {}",
            truncate_to_width(&summary, max_cols.saturating_sub(overhead))
        )
    };

    vec![
        Span::styled(format!("{icon} "), icon_style),
        Span::styled(label, label_style),
        Span::styled(summary_part, summary_style),
        Span::styled(dur_str, dur_style),
    ]
}

/// Render a single-line collapsed view for a tool result as `Vec<Span>`.
///
/// `label_override` replaces the tool display name when set (e.g. "Tool Result: shell").
pub fn render_tool_result_collapsed(
    tool_name: &str,
    is_error: bool,
    duration: Option<f32>,
    display: Option<&dyn ToolDisplay>,
    label_override: Option<String>,
) -> Vec<Span<'static>> {
    let (icon, label, category) = if let Some(d) = display {
        (
            d.icon().to_string(),
            label_override.unwrap_or_else(|| d.display_name().to_string()),
            d.category().to_string(),
        )
    } else {
        (
            sven_tools::tool_icon(tool_name).to_string(),
            label_override.unwrap_or_else(|| tool_name.to_string()),
            sven_tools::tool_category(tool_name).to_string(),
        )
    };

    let status_sym = if is_error { "✗" } else { "✓" };
    let status_color = if is_error {
        BAR_ERROR
    } else {
        Color::Rgb(80, 200, 120)
    };
    let accent = category_color(&category);

    let dur_str = if let Some(d) = duration {
        format!("  {:.1}s", d)
    } else {
        String::new()
    };

    vec![
        Span::styled(
            format!("{icon} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            label,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {status_sym}"), Style::default().fg(status_color)),
        Span::styled(dur_str, Style::default().fg(Color::Rgb(120, 120, 140))),
    ]
}

// ── Expanded tool call view ───────────────────────────────────────────────────

/// Render an expanded tool call as a list of styled lines.
///
/// Each tool category gets a distinct layout:
/// - File tools: show path with dim directory + bright filename
/// - Shell tools: show command in a code-styled block
/// - Search tools: show pattern + path with accent colours
/// - Web tools: show URL with stripped scheme
/// - System tools: show relevant args
pub fn render_tool_call_expanded(
    tool_name: &str,
    args: &serde_json::Value,
    width: u16,
    display: Option<&dyn ToolDisplay>,
) -> Vec<Line<'static>> {
    let category = display
        .map(|d| d.category().to_string())
        .unwrap_or_else(|| sven_tools::tool_category(tool_name).to_string());

    let accent = category_color(&category);

    match category.as_str() {
        "file" => render_file_tool_call(tool_name, args, width, accent),
        "shell" => render_shell_tool_call(tool_name, args, width, accent),
        "search" => render_search_tool_call(tool_name, args, width, accent),
        "web" => render_web_tool_call(tool_name, args, width, accent),
        "system" => render_system_tool_call(tool_name, args, width, accent),
        _ => render_generic_tool_call(tool_name, args, width, accent),
    }
}

/// Render an expanded tool result.
///
/// `expand` controls how much output is shown:
/// - `1` (partial): first `PARTIAL_LINES` lines followed by a "… N more" hint.
/// - `2` (full): all lines, no truncation.
pub fn render_tool_result_expanded(
    tool_name: &str,
    output: &str,
    is_error: bool,
    width: u16,
    expand: u8,
    display: Option<&dyn ToolDisplay>,
) -> Vec<Line<'static>> {
    let category = display
        .map(|d| d.category().to_string())
        .unwrap_or_else(|| sven_tools::tool_category(tool_name).to_string());

    let _accent = category_color(&category);
    let status_color = if is_error {
        BAR_ERROR
    } else {
        Color::Rgb(80, 200, 120)
    };
    let status_label = if is_error { "Error" } else { "Result" };
    let status_sym = if is_error { "✗" } else { "✓" };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header line: ✓/✗ Result
    lines.push(Line::from(vec![
        Span::styled(
            format!("{status_sym} "),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            status_label,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Output body.
    //   expand == 1 → show first PARTIAL_LINES lines with a "… N more" hint.
    //   expand >= 2 → show everything so the user can scroll the full result.
    const PARTIAL_LINES: usize = 20;
    let avail_cols = (width as usize).saturating_sub(2);
    let output_lines: Vec<&str> = output.lines().collect();
    let total = output_lines.len();

    let (to_show_count, show_hint) = if expand >= 2 {
        (total, false)
    } else {
        (total.min(PARTIAL_LINES), total > PARTIAL_LINES)
    };

    for l in output_lines.iter().take(to_show_count) {
        let display = truncate_to_width(l, avail_cols);
        lines.push(Line::from(Span::styled(
            format!("  {display}"),
            Style::default().fg(TEXT),
        )));
    }
    if show_hint {
        lines.push(Line::from(Span::styled(
            format!(
                "  … {} more lines (press Enter again to expand)",
                total - to_show_count
            ),
            Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
        )));
    }

    lines
}

// ── Category-specific call renderers ─────────────────────────────────────────

fn render_file_tool_call(
    tool_name: &str,
    args: &serde_json::Value,
    width: u16,
    accent: Color,
) -> Vec<Line<'static>> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut lines: Vec<Line<'static>> = Vec::new();

    if !path.is_empty() {
        let (dir, name) = split_path_display(&path);
        lines.push(Line::from(vec![
            Span::styled(dir, Style::default().fg(TEXT_DIM)),
            Span::styled(
                name,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // For str_replace / edit, show old/new snippets.
    if tool_name == "str_replace" || tool_name == "StrReplace" || tool_name == "str_replace_editor"
    {
        if let Some(old) = args.get("old_string").and_then(|v| v.as_str()) {
            lines.push(Line::from(Span::styled(
                "  old:",
                Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
            )));
            for l in old.lines().take(3) {
                let s = truncate_to_width(l, (width as usize).saturating_sub(4));
                lines.push(Line::from(Span::styled(
                    format!("  - {s}"),
                    Style::default().fg(Color::Rgb(220, 100, 100)),
                )));
            }
        }
        if let Some(new) = args.get("new_string").and_then(|v| v.as_str()) {
            lines.push(Line::from(Span::styled(
                "  new:",
                Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
            )));
            for l in new.lines().take(3) {
                let s = truncate_to_width(l, (width as usize).saturating_sub(4));
                lines.push(Line::from(Span::styled(
                    format!("  + {s}"),
                    Style::default().fg(Color::Rgb(80, 200, 100)),
                )));
            }
        }
    }

    // For edit_file, render the unified diff with colorized +/- lines.
    if tool_name == "edit_file" {
        if let Some(diff) = args.get("diff").and_then(|v| v.as_str()) {
            let avail = (width as usize).saturating_sub(4);
            const MAX_DIFF_LINES: usize = 30;
            let total_lines = diff.lines().count();
            for line in diff.lines().take(MAX_DIFF_LINES) {
                let (prefix, color) = if line.starts_with('+') && !line.starts_with("+++") {
                    ("+", Color::Rgb(80, 200, 100))
                } else if line.starts_with('-') && !line.starts_with("---") {
                    ("-", Color::Rgb(220, 100, 100))
                } else if line.starts_with("@@") {
                    ("@", Color::Rgb(100, 160, 255))
                } else if line.starts_with("+++") || line.starts_with("---") {
                    (" ", Color::Rgb(180, 140, 255))
                } else {
                    (" ", TEXT_DIM)
                };
                let _ = prefix; // prefix is encoded in the line itself
                let s = truncate_to_width(line, avail);
                lines.push(Line::from(Span::styled(
                    format!("  {s}"),
                    Style::default().fg(color),
                )));
            }
            if total_lines > MAX_DIFF_LINES {
                let remaining = total_lines - MAX_DIFF_LINES;
                lines.push(Line::from(Span::styled(
                    format!("  … {remaining} more lines"),
                    Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    // offset / limit for reads
    if let Some(offset) = args.get("offset").and_then(|v| v.as_i64()) {
        let limit = args.get("limit").and_then(|v| v.as_i64());
        let range_str = if let Some(lim) = limit {
            format!("  lines {offset}–{}", offset + lim)
        } else {
            format!("  from line {offset}")
        };
        lines.push(Line::from(Span::styled(
            range_str,
            Style::default().fg(TEXT_DIM),
        )));
    }

    if lines.is_empty() {
        render_generic_tool_call(tool_name, args, width, accent)
    } else {
        lines
    }
}

fn render_shell_tool_call(
    tool_name: &str,
    args: &serde_json::Value,
    width: u16,
    accent: Color,
) -> Vec<Line<'static>> {
    // Support shell_command (ShellTool), command, and cmd parameter names.
    let cmd = args
        .get("shell_command")
        .or_else(|| args.get("command"))
        .or_else(|| args.get("cmd"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Optional human-readable intent/description from the model.
    let intent = args
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if cmd.is_empty() && intent.is_none() {
        return render_generic_tool_call(tool_name, args, width, accent);
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let avail = (width as usize).saturating_sub(4);

    // If there is an intent description, show it first as a header.
    if let Some(ref desc) = intent {
        let truncated = truncate_to_width(desc, avail);
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                truncated,
                Style::default().fg(accent).add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    // Show the raw command below the intent.
    if !cmd.is_empty() {
        for (i, l) in cmd.lines().enumerate().take(5) {
            let prefix = if i == 0 { "  $ " } else { "    " };
            let s = truncate_to_width(l, avail);
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(TEXT_DIM)),
                Span::styled(s, Style::default().fg(accent).add_modifier(Modifier::BOLD)),
            ]));
        }
        if cmd.lines().count() > 5 {
            lines.push(Line::from(Span::styled(
                format!("  … {} more lines", cmd.lines().count() - 5),
                Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
            )));
        }
    }

    // Show working directory if provided; support workdir and working_directory keys.
    let wd_opt = args
        .get("workdir")
        .or_else(|| args.get("working_directory"))
        .and_then(|v| v.as_str());
    if let Some(wd) = wd_opt {
        let short_wd = sven_tools::shorten_path(wd, 3);
        lines.push(Line::from(Span::styled(
            format!("  cwd: {short_wd}"),
            Style::default().fg(TEXT_DIM),
        )));
    }

    lines
}

fn render_search_tool_call(
    tool_name: &str,
    args: &serde_json::Value,
    width: u16,
    accent: Color,
) -> Vec<Line<'static>> {
    let pattern = args
        .get("pattern")
        .or_else(|| args.get("query"))
        .or_else(|| args.get("glob_pattern"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let path = args
        .get("path")
        .or_else(|| args.get("target_directory"))
        .or_else(|| args.get("target_directories"))
        .and_then(|v| v.as_str())
        .map(|p| sven_tools::shorten_path(p, 3))
        .unwrap_or_default();

    if pattern.is_empty() {
        return render_generic_tool_call(tool_name, args, width, accent);
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let avail = (width as usize).saturating_sub(4);
    let pat = truncate_to_width(&pattern, avail);

    lines.push(Line::from(vec![
        Span::styled(
            "  / ",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            pat,
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
    ]));

    if !path.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  in: {path}"),
            Style::default().fg(TEXT_DIM),
        )));
    }

    lines
}

fn render_web_tool_call(
    tool_name: &str,
    args: &serde_json::Value,
    width: u16,
    accent: Color,
) -> Vec<Line<'static>> {
    let query_or_url = args
        .get("search_term")
        .or_else(|| args.get("query"))
        .or_else(|| args.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if query_or_url.is_empty() {
        return render_generic_tool_call(tool_name, args, width, accent);
    }

    let display_val = query_or_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string();
    let avail = (width as usize).saturating_sub(4);
    let display_val = truncate_to_width(&display_val, avail);

    vec![Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            display_val,
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
    ])]
}

fn render_system_tool_call(
    tool_name: &str,
    args: &serde_json::Value,
    width: u16,
    accent: Color,
) -> Vec<Line<'static>> {
    match tool_name {
        "todo" => {
            let mut lines: Vec<Line<'static>> = Vec::new();
            if let Some(todos) = args.get("todos").and_then(|v| v.as_array()) {
                let avail = (width as usize).saturating_sub(6);
                for todo in todos.iter().take(5) {
                    let content = todo
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let status = todo
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pending")
                        .to_string();
                    let (sym, col) = match status.as_str() {
                        "completed" => ("☑", Color::Rgb(80, 200, 120)),
                        "in_progress" => ("●", Color::Rgb(220, 180, 60)),
                        "cancelled" => ("✗", TEXT_DIM),
                        _ => ("☐", TEXT_DIM),
                    };
                    let c = truncate_to_width(&content, avail);
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {sym} "), Style::default().fg(col)),
                        Span::styled(c, Style::default().fg(TEXT)),
                    ]));
                }
                if todos.len() > 5 {
                    lines.push(Line::from(Span::styled(
                        format!("  … {} more", todos.len() - 5),
                        Style::default().fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
                    )));
                }
            }
            if !lines.is_empty() {
                return lines;
            }
        }
        "read_lints" | "ReadLints" => {
            if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
                let names: Vec<String> = paths
                    .iter()
                    .take(3)
                    .filter_map(|v| v.as_str())
                    .map(|p| sven_tools::shorten_path(p, 2))
                    .collect();
                if !names.is_empty() {
                    return vec![Line::from(Span::styled(
                        format!("  {}", names.join(", ")),
                        Style::default().fg(accent),
                    ))];
                }
            }
        }
        _ => {}
    }
    render_generic_tool_call(tool_name, args, width, accent)
}

fn render_generic_tool_call(
    _tool_name: &str,
    args: &serde_json::Value,
    width: u16,
    accent: Color,
) -> Vec<Line<'static>> {
    let avail = (width as usize).saturating_sub(4);
    if let serde_json::Value::Object(map) = args {
        map.iter()
            .take(4)
            .map(|(k, v)| {
                let val_str = match v {
                    serde_json::Value::String(s) => {
                        truncate_to_width(s, avail.saturating_sub(k.len() + 4))
                    }
                    _ => truncate_to_width(&v.to_string(), avail.saturating_sub(k.len() + 4)),
                };
                Line::from(vec![
                    Span::styled(format!("  {k}: "), Style::default().fg(TEXT_DIM)),
                    Span::styled(val_str, Style::default().fg(accent)),
                ])
            })
            .collect()
    } else {
        vec![Line::from(Span::styled(
            truncate_to_width(&args.to_string(), avail),
            Style::default().fg(accent),
        ))]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use serde_json::json;

    use super::render_file_tool_call;

    #[test]
    fn edit_file_diff_renders_colored_lines() {
        let diff = "\
--- a/foo.rs\n\
+++ b/foo.rs\n\
@@ -1,3 +1,3 @@\n\
 context\n\
-removed line\n\
+added line\n\
 context\n\
";
        let args = json!({ "path": "foo.rs", "diff": diff });
        let lines = render_file_tool_call("edit_file", &args, 80, Color::Cyan);

        // There must be at least one line for the path header and diff content.
        assert!(!lines.is_empty(), "should render at least 1 line");

        let content: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();

        // Verify that diff prefix lines are present.
        assert!(
            content.iter().any(|s| s.contains("@@")),
            "should contain hunk header: {content:?}"
        );
        assert!(
            content.iter().any(|s| s.contains("-removed line")),
            "should contain removed line: {content:?}"
        );
        assert!(
            content.iter().any(|s| s.contains("+added line")),
            "should contain added line: {content:?}"
        );
    }

    #[test]
    fn edit_file_diff_truncates_long_diffs() {
        // Create a diff with more than 30 lines.
        let mut diff = String::from("@@ -1,40 +1,40 @@\n");
        for i in 0..35 {
            diff.push_str(&format!("+added line {i}\n"));
        }
        let args = json!({ "path": "big.rs", "diff": diff });
        let lines = render_file_tool_call("edit_file", &args, 80, Color::Cyan);

        // Should have at most 30 diff lines plus path + truncation message + hunk header.
        let content: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();

        assert!(
            content.iter().any(|s| s.contains("more lines")),
            "should show truncation indicator: {content:?}"
        );
    }

    #[test]
    fn edit_file_without_diff_falls_through_to_generic() {
        // A path-only call should still render the path line.
        let args = json!({ "path": "src/main.rs" });
        let lines = render_file_tool_call("edit_file", &args, 80, Color::Yellow);
        assert!(!lines.is_empty(), "should produce at least a path line");
        let content: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect();
        assert!(content.contains("main.rs"), "path should appear: {content}");
    }
}
