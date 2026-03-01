// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use crate::markdown::StyledLines;

/// What the pager wants the app to do after handling a key.
pub enum PagerAction {
    /// Close the pager and return to normal mode.
    Close,
    /// Open the inline search bar (shared with main view).
    OpenSearch,
    /// Navigate to next search match (app updates search, then calls
    /// `scroll_to_line` with the new match).
    SearchNext,
    /// Navigate to previous search match.
    SearchPrev,
    /// Key was handled internally; nothing else needed.
    Handled,
}

/// Full-screen pager overlay with vim-style navigation.
///
/// The pager displays the current chat lines in a full-screen buffer.
/// `j`/`k`, `Ctrl+u`/`d`, `Ctrl+b`/`f`, `g`/`G`, page-keys, `Home`/`End`
/// all work as in vim/less.  `/`, `n`, `N` are forwarded to the app's
/// search machinery.  `q`/`Esc` closes the overlay.
pub struct PagerOverlay {
    lines: StyledLines,
    pub scroll_offset: usize,
    /// Detect `gg` sequence.
    last_was_g: bool,
    /// Cached visible content height from the last render frame, used for
    /// computing half-page and full-page scroll amounts.
    pub last_visible_height: usize,
}

impl PagerOverlay {
    /// Create a new pager that starts scrolled to the bottom (most recent content).
    pub fn new(lines: StyledLines) -> Self {
        Self {
            lines,
            scroll_offset: usize::MAX,
            last_was_g: false,
            last_visible_height: 24,
        }
    }

    /// Replace the displayed lines (e.g. after a chat update while pager is open).
    pub fn set_lines(&mut self, lines: StyledLines) {
        self.lines = lines;
    }

    /// Scroll the pager so that `line` is visible near the top.
    pub fn scroll_to_line(&mut self, line: usize) {
        self.scroll_offset = line;
    }

    /// Returns the clamped scroll offset for the given visible height.
    fn clamped_offset(&self, visible_height: usize) -> usize {
        let max = self.lines.len().saturating_sub(visible_height);
        if self.scroll_offset == usize::MAX {
            max
        } else {
            self.scroll_offset.min(max)
        }
    }

    fn scroll_up(&mut self, n: usize) {
        if self.scroll_offset == usize::MAX {
            let max = self.lines.len().saturating_sub(self.last_visible_height);
            self.scroll_offset = max;
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    fn scroll_down(&mut self, n: usize) {
        if self.scroll_offset == usize::MAX {
            return; // already at bottom
        }
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Handle a key event.  Returns the [`PagerAction`] the app should perform.
    pub fn handle_key(&mut self, event: KeyEvent) -> PagerAction {
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        let was_g = self.last_was_g;
        self.last_was_g = false;

        let half = self.last_visible_height.div_ceil(2);
        let full = self.last_visible_height.max(1);

        match event.code {
            // ── Close ─────────────────────────────────────────────────────────
            KeyCode::Char('q') | KeyCode::Esc => return PagerAction::Close,

            // ── Line scrolling ────────────────────────────────────────────────
            KeyCode::Char('j') | KeyCode::Down => self.scroll_down(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_up(1),
            KeyCode::Char('J') => self.scroll_down(1),
            KeyCode::Char('K') => self.scroll_up(1),

            // ── Half-page ─────────────────────────────────────────────────────
            KeyCode::Char('d') if ctrl => self.scroll_down(half),
            KeyCode::Char('u') if ctrl => self.scroll_up(half),

            // ── Full-page ─────────────────────────────────────────────────────
            KeyCode::Char('f') if ctrl => self.scroll_down(full),
            KeyCode::PageDown => self.scroll_down(full),
            KeyCode::Char('b') if ctrl => self.scroll_up(full),
            KeyCode::PageUp => self.scroll_up(full),

            // ── Jump to bottom ────────────────────────────────────────────────
            KeyCode::Char('G') => self.scroll_offset = usize::MAX,
            KeyCode::End => self.scroll_offset = usize::MAX,

            // ── `gg` → jump to top ────────────────────────────────────────────
            KeyCode::Char('g') if !ctrl => {
                if was_g {
                    self.scroll_offset = 0;
                } else {
                    self.last_was_g = true;
                }
            }
            KeyCode::Home => self.scroll_offset = 0,

            // ── Search forwarding ─────────────────────────────────────────────
            KeyCode::Char('/') => return PagerAction::OpenSearch,
            KeyCode::Char('n') => return PagerAction::SearchNext,
            KeyCode::Char('N') => return PagerAction::SearchPrev,

            _ => {}
        }

        PagerAction::Handled
    }

    /// Render the pager overlay over the entire terminal area.
    ///
    /// `search_matches` and `search_current` come from the app's `SearchState`.
    /// `search_query` is used to highlight matching text in the current match line.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        search_matches: &[usize],
        search_current: usize,
        search_query: &str,
        search_regex: Option<&regex::Regex>,
        ascii: bool,
    ) {
        let area = frame.area();
        let content_h = area.height.saturating_sub(3); // 1 header + 2 footer
        let content_area = Rect::new(area.x, area.y + 1, area.width, content_h);
        let sep_area = Rect::new(area.x, area.y + 1 + content_h, area.width, 1);
        let hints_area = Rect::new(area.x, area.y + 2 + content_h, area.width, 1);

        self.last_visible_height = content_h as usize;

        // Clear the whole screen for the overlay
        frame.render_widget(Clear, area);

        // ── Header ────────────────────────────────────────────────────────────
        let dash = if ascii { "-" } else { "╌" };
        let half_w = (area.width as usize / 2).saturating_sub(7);
        let fill = dash.repeat(half_w);
        let header = Line::from(vec![
            Span::styled(fill.clone(), Style::default().fg(Color::DarkGray)),
            Span::styled(
                " TRANSCRIPT ",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(fill, Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(
            Paragraph::new(header),
            Rect::new(area.x, area.y, area.width, 1),
        );

        // ── Content ───────────────────────────────────────────────────────────
        let visible_height = content_h as usize;
        let offset = self.clamped_offset(visible_height);

        // Build a set of match lines for O(1) lookup
        let match_set: std::collections::HashSet<usize> = search_matches.iter().copied().collect();
        let current_match_line: Option<usize> = search_matches.get(search_current).copied();

        let visible_lines: Vec<Line<'static>> = (offset..offset + visible_height)
            .map(|i| match self.lines.get(i) {
                Some(line) => {
                    let is_current = !search_query.is_empty() && current_match_line == Some(i);
                    let is_other_match =
                        !search_query.is_empty() && match_set.contains(&i) && !is_current;
                    if is_current {
                        highlight_match_in_line(line.clone(), search_query, search_regex)
                    } else if is_other_match {
                        tint_match_line(line.clone())
                    } else {
                        line.clone()
                    }
                }
                // Lines below content: vim-style `~`
                None => Line::from(Span::styled("~", Style::default().fg(Color::DarkGray))),
            })
            .collect();

        frame.render_widget(Paragraph::new(visible_lines), content_area);

        // ── Footer separator with scroll percentage ───────────────────────────
        let total = self.lines.len();
        let percent: u8 = if total == 0 || visible_height >= total {
            100
        } else {
            let max_scroll = total.saturating_sub(visible_height);
            let pos = offset.min(max_scroll);
            ((pos as f64 / max_scroll as f64) * 100.0).round() as u8
        };

        let sep_char = if ascii { "-" } else { "─" };
        let pct_str = format!(" {percent}% ");
        let sep_width = area.width.saturating_sub(pct_str.len() as u16) as usize;
        let sep_fill = sep_char.repeat(sep_width);
        let sep_line = Line::from(vec![
            Span::styled(sep_fill, Style::default().fg(Color::DarkGray)),
            Span::styled(pct_str, Style::default().fg(Color::Gray)),
        ]);
        frame.render_widget(Paragraph::new(sep_line), sep_area);

        // ── Key hints ─────────────────────────────────────────────────────────
        let k = Style::default().fg(Color::Gray);
        let d = Style::default().fg(Color::DarkGray);
        let hints = Line::from(vec![
            Span::styled(" j/k/J/K", k),
            Span::styled(":line  ", d),
            Span::styled("^u/^d", k),
            Span::styled(":½pg  ", d),
            Span::styled("^b/^f", k),
            Span::styled(":page  ", d),
            Span::styled("gg/G", k),
            Span::styled(":top/bot  ", d),
            Span::styled("/", k),
            Span::styled(":search  ", d),
            Span::styled("n/N", k),
            Span::styled(":match  ", d),
            Span::styled("q", k),
            Span::styled(":close", d),
        ]);
        frame.render_widget(Paragraph::new(hints), hints_area);
    }
}

// ─── Search highlighting helpers ──────────────────────────────────────────────

/// Re-style a line so that occurrences of `query` are shown with a yellow
/// background.  Used for the *current* search match line.
pub(crate) fn highlight_match_in_line(
    line: Line<'static>,
    query: &str,
    re: Option<&regex::Regex>,
) -> Line<'static> {
    let match_style = Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let mut new_spans: Vec<Span<'static>> = Vec::new();

    for span in line.spans {
        let text: &str = &span.content;
        let base_style = span.style;

        // Collect match byte ranges in `text`
        let matches: Vec<(usize, usize)> = if let Some(re) = re {
            re.find_iter(text).map(|m| (m.start(), m.end())).collect()
        } else {
            let q = query.to_lowercase();
            let lower = text.to_lowercase();
            let mut ms = Vec::new();
            let mut pos = 0usize;
            while pos < lower.len() {
                match lower[pos..].find(&q) {
                    Some(rel) if !q.is_empty() => {
                        let start = pos + rel;
                        let end = start + q.len();
                        ms.push((start, end));
                        pos = end;
                    }
                    _ => break,
                }
            }
            ms
        };

        let mut last = 0usize;
        for (start, end) in &matches {
            if *start > last {
                new_spans.push(Span::styled(text[last..*start].to_string(), base_style));
            }
            new_spans.push(Span::styled(text[*start..*end].to_string(), match_style));
            last = *end;
        }
        // Remaining text (or full text when there are no matches)
        if last < text.len() {
            new_spans.push(Span::styled(text[last..].to_string(), base_style));
        }
    }

    Line::from(new_spans)
}

/// Apply a subtle background tint to a line that contains a *non-current*
/// search match.
pub(crate) fn tint_match_line(line: Line<'static>) -> Line<'static> {
    let tint = Style::default().bg(Color::DarkGray);
    Line::from(
        line.spans
            .into_iter()
            .map(|s| Span::styled(s.content, s.style.patch(tint)))
            .collect::<Vec<_>>(),
    )
}
