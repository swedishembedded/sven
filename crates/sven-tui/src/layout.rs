use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

/// The regions that make up the TUI layout.
#[derive(Debug, Clone, Copy)]
pub struct AppLayout {
    pub status_bar: Rect,
    pub chat_pane: Rect,
    pub input_pane: Rect,
    pub search_bar: Rect,
}

impl AppLayout {
    /// Calculate layout regions from a `Rect` (terminal area).
    pub fn compute(area: Rect, search_visible: bool) -> Self {
        let status_height = 1u16;
        let input_height = 5u16;
        let search_height = if search_visible { 1u16 } else { 0u16 };

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(status_height),
                Constraint::Min(10),
                Constraint::Length(input_height),
                Constraint::Length(search_height),
            ])
            .split(area);

        AppLayout {
            status_bar: vertical[0],
            chat_pane: vertical[1],
            input_pane: vertical[2],
            search_bar: vertical[3],
        }
    }

    /// Convenience wrapper — derive the area from the current frame.
    pub fn new(frame: &Frame, search_visible: bool) -> Self {
        Self::compute(frame.area(), search_visible)
    }

    /// The number of text rows visible inside the chat pane's border.
    /// (pane height minus the two border rows)
    pub fn chat_inner_height(&self) -> u16 {
        self.chat_pane.height.saturating_sub(2)
    }
}
