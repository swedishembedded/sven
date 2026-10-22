// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::FocusPane;

/// All logical actions the TUI can perform, independent of key binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // Navigation
    FocusInput,
    /// First key of the Ctrl+w nav chord (vim-style window navigation).
    NavPrefix,
    /// Navigate to the pane above the current one (Ctrl+w k).
    NavUp,
    /// Navigate to the pane below the current one (Ctrl+w j).
    NavDown,
    /// Navigate to the pane to the left of the current one (Ctrl+w h).
    NavLeft,
    /// Navigate to the pane to the right of the current one (Ctrl+w l).
    NavRight,
    /// Focus the activity panel listing the session's subagents (Ctrl+w a).
    FocusActivity,

    // Activity panel
    /// Highlight the previous / next agent in the open panel.
    ActivityNavUp,
    ActivityNavDown,
    /// Open the panel to a row per agent, or fold it (Space).
    ToggleActivity,
    /// Open a folded panel; show the highlighted agent's session (Enter).
    ActivateActivity,
    /// Fold the panel and return to the input (Esc, q).
    LeaveActivity,

    // Scrolling (in chat pane)
    ScrollUp,
    ScrollDown,
    /// Move the chat highlight to the next segment (j when chat has focus).
    ChatHighlightDown,
    /// Move the chat highlight to the previous segment (k when chat has focus).
    ChatHighlightUp,
    /// Show the chat shortcuts help modal (Enter when chat has focus).
    ShowChatHelp,
    ScrollPageUp,
    ScrollPageDown,
    /// PgUp/PgDn: scroll the chat pane a full page, regardless of which pane
    /// has focus (distinct from `ScrollPageUp`/`ScrollPageDown`'s vim-style
    /// half-page `Ctrl+u`/`Ctrl+d`).
    ScrollFullPageUp,
    ScrollFullPageDown,
    ScrollTop,
    ScrollBottom,

    // Search
    SearchOpen,
    SearchClose,
    SearchNextMatch,
    SearchPrevMatch,
    SearchInput(char),
    SearchBackspace,

    // Input
    InputChar(char),
    InputNewline,
    /// ESC while focused on the input pane (not in a completion overlay).
    /// Cancels an ongoing edit when one is active; otherwise clears the input
    /// buffer, attachments, and resets scroll/history navigation state.
    InputEscape,
    InputBackspace,
    InputDelete,
    InputMoveCursorLeft,
    InputMoveCursorRight,
    InputMoveWordLeft,
    InputMoveWordRight,
    InputMoveLineStart,
    InputMoveLineEnd,
    /// Move cursor up one visual row; when already on the first row, cycle to older history.
    InputMoveLineUp,
    /// Move cursor down one visual row; when already on the last row, cycle to newer history.
    InputMoveLineDown,
    InputPageUp,
    InputPageDown,
    InputDeleteToEnd,
    InputDeleteToStart,
    /// Navigate backwards through input history (older messages). Ctrl+Up always jumps.
    InputHistoryUp,
    /// Navigate forwards through input history (newer messages). Ctrl+Down always jumps.
    InputHistoryDown,
    Submit,

    // Agent
    InterruptAgent,
    CycleMode,

    // Edit message (inline edit mode)
    EditMessageAtCursor,
    EditMessageConfirm,
    EditMessageCancel,
    /// Delete the currently selected queued message.
    DeleteQueuedMessage,
    /// Truncate chat history from the focused segment onward (chat pane, `d`).
    DeleteChatSegment,
    /// Remove only the focused segment (and its paired ToolCall/Result if applicable).
    RemoveChatSegment,
    /// Truncate to just before the focused segment and re-submit to the agent.
    RerunFromSegment,
    /// Focus the queue panel (shown above the input when there are queued messages).
    FocusQueue,
    /// Navigate the queue panel selection up.
    QueueNavUp,
    /// Navigate the queue panel selection down.
    QueueNavDown,
    /// Start editing the currently selected queued message.
    QueueEditSelected,
    /// Submit the selected queued message immediately, even if the agent is busy.
    ForceSubmitQueuedMessage,
    /// Submit the selected queued message when the agent is idle.
    QueueSubmitSelected,

    // Input pane resize
    /// Grow the input pane by one row.
    ResizeInputGrow,
    /// Shrink the input pane by one row.
    ResizeInputShrink,

    // Buffer submit (Neovim integration)
    SubmitBufferToAgent,

    // Completion overlay
    CompletionNext,
    CompletionPrev,
    CompletionSelect,
    CompletionCancel,

    // App
    Help,
    OpenPager,

    // Clipboard
    /// Copy the focused segment's text to the system clipboard (y in chat pane).
    CopySegment,
    /// Copy all chat content to the system clipboard (Y in chat pane).
    CopyAll,

    // Team / multi-agent
    /// Toggle the team picker overlay (Ctrl+a).
    OpenTeamPicker,
    /// Navigate down in the team picker list.
    TeamPickerNext,
    /// Navigate up in the team picker list.
    TeamPickerPrev,
    /// Confirm selection in the team picker (Enter).
    TeamPickerSelect,
    /// Close the team picker without switching (Esc).
    TeamPickerClose,
    /// Cycle the active view forward to the next teammate (Shift+Down).
    CycleTeammateForward,
    /// Cycle the active view backward to the previous teammate (Shift+Up).
    CycleTeammateBackward,
    /// Toggle the task list overlay (Ctrl+t when in team mode).
    ToggleTaskList,
    /// Expand or collapse the highlighted chat item (Space, a click).
    ToggleChatItem,
    /// Expand the highlighted item; on an expanded run, step into it (l, →).
    ExpandChatItem,
    /// Collapse the highlighted item, or the run it is in (h, ←).
    CollapseChatItem,
    /// Open the highlighted item in full in the viewer (o).
    OpenChatItem,
    /// Enter on the highlighted item: edit a message, expand anything else.
    ActivateChatItem,

    // Session picker (`/resume`)
    /// Navigate down in the session picker list.
    SessionPickerNext,
    /// Navigate up in the session picker list.
    SessionPickerPrev,
    /// Reload the selected session and close the picker (Enter).
    SessionPickerSelect,
    /// Close the session picker without switching (Esc).
    SessionPickerClose,

    // ── Mouse-originated actions ──────────────────────────────────────────────
    // These are produced by `App::mouse_to_action` and dispatched like any
    // keyboard action so that all state mutations live in `dispatch.rs`.
    /// Click on a row in the queue panel.
    QueueClick {
        index: usize,
    },

    /// Click on the activity panel, `row` rows below its top.
    ActivityClick {
        row: u16,
    },

    /// Click anywhere in the chat content area.
    ///
    /// Dispatching this action sets the selection anchor **and**, if the click
    /// falls on a collapsible segment body, cycles its expand level.
    ChatContentClick {
        abs_line: usize,
        inner_col: u16,
    },

    /// Mouse drag extending an in-progress text selection.
    ///
    /// `abs_line` and `inner_col` are the clamped pointer position (already
    /// bounded to the chat content area).  `mouse_row` is the **raw** terminal
    /// row used to detect the auto-scroll zones near the top/bottom edge.
    SelectionExtend {
        abs_line: usize,
        inner_col: u16,
        mouse_row: u16,
    },

    /// Mouse button released - finalise and copy any active selection.
    SelectionFinish,

    /// Clear any in-progress or completed selection (e.g. click outside chat).
    SelectionClear,

    /// Scroll-wheel up while the pointer is over the input pane (moves cursor).
    InputScrollUp,
    /// Scroll-wheel down while the pointer is over the input pane (moves cursor).
    InputScrollDown,

    /// Scroll-wheel up while Neovim bridge is active (forwarded as `<C-y>`).
    NvimScrollUp,
    /// Scroll-wheel down while Neovim bridge is active (forwarded as `<C-e>`).
    NvimScrollDown,
}

/// What a key press lands on: the pane with focus and the modes in force.
#[derive(Debug, Clone, Copy)]
pub struct KeyContext {
    pub focus: FocusPane,
    /// The search bar is open.
    pub searching: bool,
    /// A Ctrl+w prefix has been received but not yet resolved; only the
    /// chord's second keys (and Esc to cancel) are meaningful.
    pub pending_nav: bool,
    /// A message or queued message is being edited; Enter/Esc confirm/cancel.
    pub editing: bool,
}

/// Map a raw key event to an [`Action`], depending on what has focus.
pub fn map_key(event: KeyEvent, cx: KeyContext) -> Option<Action> {
    let in_search = cx.searching;
    let in_input = cx.focus == FocusPane::Input;
    let pending_nav = cx.pending_nav;
    let in_edit_mode = cx.editing;
    let in_queue = cx.focus == FocusPane::Queue;
    let in_chat_pane = cx.focus == FocusPane::Chat;
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    // "plain" = no modifier that would make a char a control sequence
    let plain = !ctrl && !alt;

    // ── Pending Ctrl+w chord ──────────────────────────────────────────────────
    if pending_nav {
        return match event.code {
            KeyCode::Char('k') | KeyCode::Up => Some(Action::NavUp),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::NavDown),
            KeyCode::Char('h') | KeyCode::Left => Some(Action::NavLeft),
            KeyCode::Char('l') | KeyCode::Right => Some(Action::NavRight),
            KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::ResizeInputGrow),
            KeyCode::Char('-') => Some(Action::ResizeInputShrink),
            KeyCode::Char('a') => Some(Action::FocusActivity),
            _ => None, // cancel without action
        };
    }

    // ── PgUp/PgDn: scroll the chat output regardless of focus ────────────────
    // These fire before every focus-specific block below (search, edit mode
    // excepted - see below, queue, chat list, input) so paging the output
    // works no matter which pane has keyboard focus. Alt+PgUp/PgDn are left
    // free for paging a text buffer directly (input box, edit box).
    if !in_edit_mode && plain {
        match event.code {
            KeyCode::PageUp => return Some(Action::ScrollFullPageUp),
            KeyCode::PageDown => return Some(Action::ScrollFullPageDown),
            _ => {}
        }
    }

    if in_search {
        return map_search_key(event);
    }

    // ── Edit message mode ─────────────────────────────────────────────────────
    if in_edit_mode {
        return match event.code {
            // Alt+Enter is universal; Shift/Ctrl+Enter need keyboard enhancement.
            KeyCode::Enter if alt || shift || ctrl => Some(Action::InputNewline),
            KeyCode::Enter => Some(Action::EditMessageConfirm),
            // Ctrl+J (0x0A) is universally distinct from Enter (0x0D).
            KeyCode::Char('j') if ctrl => Some(Action::InputNewline),
            KeyCode::Esc => Some(Action::EditMessageCancel),
            KeyCode::Backspace => Some(Action::InputBackspace),
            KeyCode::Delete => Some(Action::InputDelete),
            KeyCode::Left if ctrl => Some(Action::InputMoveWordLeft),
            KeyCode::Right if ctrl => Some(Action::InputMoveWordRight),
            KeyCode::Left => Some(Action::InputMoveCursorLeft),
            KeyCode::Right => Some(Action::InputMoveCursorRight),
            KeyCode::Up => Some(Action::InputMoveLineUp),
            KeyCode::Down => Some(Action::InputMoveLineDown),
            KeyCode::PageUp => Some(Action::InputPageUp),
            KeyCode::PageDown => Some(Action::InputPageDown),
            KeyCode::Home => Some(Action::InputMoveLineStart),
            KeyCode::End => Some(Action::InputMoveLineEnd),
            KeyCode::Char('u') if ctrl => Some(Action::InputDeleteToStart),
            KeyCode::Char('k') if ctrl => Some(Action::InputDeleteToEnd),
            KeyCode::Char(c) if plain => Some(Action::InputChar(c)),
            _ => None,
        };
    }

    // ── Activity panel focus ──────────────────────────────────────────────────
    if cx.focus == FocusPane::Activity {
        return match event.code {
            KeyCode::Up | KeyCode::Char('k') => Some(Action::ActivityNavUp),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::ActivityNavDown),
            KeyCode::Enter => Some(Action::ActivateActivity),
            KeyCode::Char(' ') => Some(Action::ToggleActivity),
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::LeaveActivity),
            KeyCode::Char('w') if ctrl => Some(Action::NavPrefix),
            _ => None,
        };
    }

    // ── Queue panel focus ─────────────────────────────────────────────────────
    if in_queue {
        return match event.code {
            KeyCode::Up | KeyCode::Char('k') => Some(Action::QueueNavUp),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::QueueNavDown),
            KeyCode::Char('e') => Some(Action::QueueEditSelected),
            KeyCode::Enter => Some(Action::ForceSubmitQueuedMessage),
            KeyCode::Char('d') | KeyCode::Delete => Some(Action::DeleteQueuedMessage),
            KeyCode::Char('f') => Some(Action::ForceSubmitQueuedMessage),
            KeyCode::Char('s') => Some(Action::QueueSubmitSelected),
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::FocusInput),
            KeyCode::Char('w') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::NavPrefix)
            }
            _ => None,
        };
    }

    match event.code {
        // ── Input-pane overrides come FIRST ───────────────────────────────────
        KeyCode::Char('c') if ctrl && in_input => Some(Action::InterruptAgent),
        KeyCode::Char('u') if ctrl && in_input => Some(Action::InputDeleteToStart),
        KeyCode::Char('k') if ctrl && in_input => Some(Action::InputDeleteToEnd),
        // Ctrl+Up/Down: explicit history navigation (always jumps, regardless of cursor row).
        // Plain Up/Down also trigger history when the cursor is already on the first/last row
        // (shell-style), which is handled inside the InputMoveLineUp/Down dispatch handlers.
        KeyCode::Up if ctrl && in_input => Some(Action::InputHistoryUp),
        KeyCode::Down if ctrl && in_input => Some(Action::InputHistoryDown),

        // ── Global bindings ───────────────────────────────────────────────────
        KeyCode::Char('w') if ctrl => Some(Action::NavPrefix),
        KeyCode::F(1) => Some(Action::Help),
        KeyCode::F(4) => Some(Action::CycleMode),
        KeyCode::Char('t') if ctrl => Some(Action::OpenPager),
        // Team / multi-agent controls
        KeyCode::Char('a') if ctrl => Some(Action::OpenTeamPicker),
        KeyCode::Down if shift => Some(Action::CycleTeammateForward),
        KeyCode::Up if shift => Some(Action::CycleTeammateBackward),
        // Alt+t - task list (distinct from Ctrl+t which opens the chat pager).
        KeyCode::Char('t') if alt => Some(Action::ToggleTaskList),

        // ── Input pane ────────────────────────────────────────────────────────
        // ESC in the input pane: cancel ongoing edit, or clear the input box.
        // (Completion-overlay ESC is handled earlier in term_events.rs and never
        // reaches this point.)
        KeyCode::Esc if in_input => Some(Action::InputEscape),
        KeyCode::Tab if in_input && !shift => Some(Action::CompletionNext),
        KeyCode::BackTab if in_input => Some(Action::CompletionPrev),
        KeyCode::Enter if in_input && !shift && !ctrl && !alt => Some(Action::Submit),
        // Alt+Enter is the universal newline shortcut (works in every terminal).
        // Shift/Ctrl+Enter also work when the Kitty keyboard protocol is active.
        KeyCode::Enter if in_input && alt => Some(Action::InputNewline),
        KeyCode::Enter if in_input && shift => Some(Action::InputNewline),
        KeyCode::Enter if in_input && ctrl => Some(Action::InputNewline),
        // Ctrl+J (byte 0x0A) is universally distinct from Enter (0x0D) in raw mode.
        KeyCode::Char('j') if ctrl && in_input => Some(Action::InputNewline),
        KeyCode::Backspace if in_input => Some(Action::InputBackspace),
        KeyCode::Delete if in_input => Some(Action::InputDelete),
        KeyCode::Left if in_input && ctrl => Some(Action::InputMoveWordLeft),
        KeyCode::Right if in_input && ctrl => Some(Action::InputMoveWordRight),
        KeyCode::Left if in_input => Some(Action::InputMoveCursorLeft),
        KeyCode::Right if in_input => Some(Action::InputMoveCursorRight),
        KeyCode::Up if in_input => Some(Action::InputMoveLineUp),
        KeyCode::Down if in_input => Some(Action::InputMoveLineDown),
        // Plain PageUp/PageDown are claimed globally above (scroll the chat
        // output). Alt+PageUp/PageDown still page the input buffer itself.
        KeyCode::PageUp if in_input && alt => Some(Action::InputPageUp),
        KeyCode::PageDown if in_input && alt => Some(Action::InputPageDown),
        KeyCode::Home if in_input => Some(Action::InputMoveLineStart),
        KeyCode::End if in_input => Some(Action::InputMoveLineEnd),
        KeyCode::Char(c) if in_input && plain => Some(Action::InputChar(c)),

        // ── Chat pane ─────────────────────────────────────────────────────────
        // j/k move the highlight; J/K scroll.
        KeyCode::Up | KeyCode::Char('k') if !in_input && plain && in_chat_pane => {
            Some(Action::ChatHighlightUp)
        }
        KeyCode::Down | KeyCode::Char('j') if !in_input && plain && in_chat_pane => {
            Some(Action::ChatHighlightDown)
        }
        KeyCode::Enter if !in_input && plain && in_chat_pane => Some(Action::ActivateChatItem),
        KeyCode::Right | KeyCode::Char('l') if !in_input && plain && in_chat_pane => {
            Some(Action::ExpandChatItem)
        }
        KeyCode::Left | KeyCode::Char('h') if !in_input && plain && in_chat_pane => {
            Some(Action::CollapseChatItem)
        }
        KeyCode::Char('o') if !in_input && plain && in_chat_pane => Some(Action::OpenChatItem),
        KeyCode::Char('K') if !in_input => Some(Action::ScrollUp),
        KeyCode::Char('J') if !in_input => Some(Action::ScrollDown),
        KeyCode::Char('u') if ctrl && !in_input => Some(Action::ScrollPageUp),
        KeyCode::Char('d') if ctrl && !in_input => Some(Action::ScrollPageDown),
        KeyCode::Char('g') if !in_input && plain => Some(Action::ScrollTop),
        KeyCode::Char('G') if !in_input => Some(Action::ScrollBottom),

        // Search
        KeyCode::Char('/') if !in_input && plain => Some(Action::SearchOpen),
        KeyCode::Char('n') if !in_input && plain => Some(Action::SearchNextMatch),
        KeyCode::Char('N') if !in_input => Some(Action::SearchPrevMatch),

        // Help
        KeyCode::Char('?') if !in_input => Some(Action::ShowChatHelp),

        // Edit / delete / copy focused segment
        KeyCode::Char('e') if !in_input && plain => Some(Action::EditMessageAtCursor),
        KeyCode::F(2) if !in_input => Some(Action::EditMessageAtCursor),
        KeyCode::Char('d') if !in_input && plain => Some(Action::DeleteChatSegment),
        KeyCode::F(8) if !in_input => Some(Action::DeleteChatSegment),
        KeyCode::Char('x') if !in_input && plain => Some(Action::RemoveChatSegment),
        KeyCode::Char('r') if !in_input && plain => Some(Action::RerunFromSegment),
        KeyCode::Char('y') if !in_input && plain => Some(Action::CopySegment),
        KeyCode::Char('Y') if !in_input => Some(Action::CopyAll),
        KeyCode::Char('q') if !in_input && plain => Some(Action::FocusQueue),

        // Space in the chat pane expands or collapses the highlighted item.
        KeyCode::Char(' ') if !in_input && plain => Some(Action::ToggleChatItem),

        // Submit buffer to agent (Ctrl+Enter from chat pane with Neovim)
        KeyCode::Enter if !in_input && ctrl => Some(Action::SubmitBufferToAgent),

        _ => None,
    }
}

pub(crate) fn map_search_key(event: KeyEvent) -> Option<Action> {
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);

    match event.code {
        KeyCode::Esc => Some(Action::SearchClose),
        KeyCode::Enter => Some(Action::SearchClose),
        KeyCode::Backspace => Some(Action::SearchBackspace),
        KeyCode::Char('n') if !shift => Some(Action::SearchNextMatch),
        KeyCode::Char('N') | KeyCode::Char('n') if shift => Some(Action::SearchPrevMatch),
        KeyCode::Char(c) => Some(Action::SearchInput(c)),
        _ => None,
    }
}

/// Map a raw key to a `SessionPicker*` action while the `/resume` overlay is open.
pub(crate) fn map_session_picker_key(code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => Some(Action::SessionPickerClose),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::SessionPickerNext),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::SessionPickerPrev),
        KeyCode::Enter => Some(Action::SessionPickerSelect),
        _ => None,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn plain_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn on(focus: FocusPane) -> KeyContext {
        KeyContext {
            focus,
            searching: false,
            pending_nav: false,
            editing: false,
        }
    }
    fn chat() -> KeyContext {
        on(FocusPane::Chat)
    }
    fn input() -> KeyContext {
        on(FocusPane::Input)
    }
    fn queue() -> KeyContext {
        on(FocusPane::Queue)
    }
    fn pending() -> KeyContext {
        KeyContext {
            pending_nav: true,
            ..chat()
        }
    }
    fn editing(focus: FocusPane) -> KeyContext {
        KeyContext {
            editing: true,
            ..on(focus)
        }
    }

    #[test]
    fn ctrl_w_returns_nav_prefix() {
        let ev = ctrl_key('w');
        assert_eq!(map_key(ev, chat()), Some(Action::NavPrefix));
        assert_eq!(map_key(ev, input()), Some(Action::NavPrefix));
    }

    #[test]
    fn pending_nav_k_returns_nav_up() {
        assert_eq!(map_key(plain_key('k'), pending()), Some(Action::NavUp));
    }

    #[test]
    fn pending_nav_j_returns_nav_down() {
        assert_eq!(map_key(plain_key('j'), pending()), Some(Action::NavDown));
    }

    #[test]
    fn pending_nav_h_returns_nav_left() {
        assert_eq!(map_key(plain_key('h'), pending()), Some(Action::NavLeft));
    }

    #[test]
    fn pending_nav_l_returns_nav_right() {
        assert_eq!(map_key(plain_key('l'), pending()), Some(Action::NavRight));
    }

    #[test]
    fn pending_nav_plus_grows_input() {
        assert_eq!(
            map_key(plain_key('+'), pending()),
            Some(Action::ResizeInputGrow)
        );
    }

    #[test]
    fn pending_nav_minus_shrinks_input() {
        assert_eq!(
            map_key(plain_key('-'), pending()),
            Some(Action::ResizeInputShrink)
        );
    }

    #[test]
    fn ctrl_up_in_input_is_history_up() {
        let ev = key(KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(map_key(ev, input()), Some(Action::InputHistoryUp));
    }

    #[test]
    fn ctrl_down_in_input_is_history_down() {
        let ev = key(KeyCode::Down, KeyModifiers::CONTROL);
        assert_eq!(map_key(ev, input()), Some(Action::InputHistoryDown));
    }

    #[test]
    fn plain_up_in_input_is_move_line_up() {
        let ev = key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(map_key(ev, input()), Some(Action::InputMoveLineUp));
    }

    #[test]
    fn plain_down_in_input_is_move_line_down() {
        let ev = key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(map_key(ev, input()), Some(Action::InputMoveLineDown));
    }

    #[test]
    fn pending_nav_other_key_cancels() {
        assert_eq!(map_key(plain_key('x'), pending()), None);
    }

    #[test]
    fn ctrl_w_in_input_does_not_type_w() {
        let ev = ctrl_key('w');
        let action = map_key(ev, input());
        assert_ne!(action, Some(Action::InputChar('w')));
        assert_eq!(action, Some(Action::NavPrefix));
    }

    #[test]
    fn plain_char_in_input_types() {
        // 'h' is NavLeft in pending_nav, but plain 'h' in input types normally.
        assert_eq!(
            map_key(plain_key('h'), input()),
            Some(Action::InputChar('h'))
        );
    }

    #[test]
    fn plain_char_x_outside_input_removes_segment() {
        assert_eq!(
            map_key(plain_key('x'), chat()),
            Some(Action::RemoveChatSegment)
        );
    }

    #[test]
    fn ctrl_k_in_input_deletes_to_end() {
        assert_eq!(
            map_key(ctrl_key('k'), input()),
            Some(Action::InputDeleteToEnd)
        );
    }

    #[test]
    fn ctrl_k_in_chat_does_not_fire() {
        assert_eq!(map_key(ctrl_key('k'), chat()), None);
    }

    #[test]
    fn ctrl_c_outside_input_not_reserved() {
        assert_eq!(map_key(ctrl_key('c'), chat()), None);
    }

    #[test]
    fn ctrl_c_interrupts_inside_input() {
        assert_eq!(
            map_key(ctrl_key('c'), input()),
            Some(Action::InterruptAgent)
        );
    }

    #[test]
    fn j_in_chat_pane_moves_highlight_down() {
        assert_eq!(
            map_key(plain_key('j'), chat()),
            Some(Action::ChatHighlightDown)
        );
    }

    #[test]
    fn ctrl_u_in_chat_page_up() {
        assert_eq!(map_key(ctrl_key('u'), chat()), Some(Action::ScrollPageUp));
    }

    #[test]
    fn e_in_chat_opens_edit() {
        assert_eq!(
            map_key(plain_key('e'), chat()),
            Some(Action::EditMessageAtCursor)
        );
    }

    #[test]
    fn edit_mode_enter_confirms() {
        let ev = key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, editing(FocusPane::Input)),
            Some(Action::EditMessageConfirm)
        );
    }

    #[test]
    fn edit_mode_esc_cancels() {
        let ev = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, editing(FocusPane::Input)),
            Some(Action::EditMessageCancel)
        );
    }

    #[test]
    fn edit_mode_char_goes_to_input() {
        assert_eq!(
            map_key(plain_key('x'), editing(FocusPane::Input)),
            Some(Action::InputChar('x'))
        );
    }

    // ── Team / multi-agent key bindings ───────────────────────────────────────

    #[test]
    fn ctrl_a_opens_team_picker() {
        assert_eq!(map_key(ctrl_key('a'), chat()), Some(Action::OpenTeamPicker));
    }

    #[test]
    fn ctrl_a_in_input_opens_team_picker() {
        assert_eq!(
            map_key(ctrl_key('a'), input()),
            Some(Action::OpenTeamPicker)
        );
    }

    #[test]
    fn shift_down_cycles_teammate_forward() {
        let ev = key(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(map_key(ev, chat()), Some(Action::CycleTeammateForward));
    }

    #[test]
    fn shift_up_cycles_teammate_backward() {
        let ev = key(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(map_key(ev, chat()), Some(Action::CycleTeammateBackward));
    }

    #[test]
    fn alt_t_opens_task_list() {
        let ev = key(KeyCode::Char('t'), KeyModifiers::ALT);
        assert_eq!(map_key(ev, chat()), Some(Action::ToggleTaskList));
    }

    #[test]
    fn space_in_chat_toggles_the_highlighted_item() {
        let ev = key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(map_key(ev, chat()), Some(Action::ToggleChatItem));
    }

    #[test]
    fn space_in_input_pane_types_char() {
        let ev = key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(map_key(ev, input()), Some(Action::InputChar(' ')));
    }

    #[test]
    fn shift_down_in_input_pane_is_history_down() {
        let ev = key(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(map_key(ev, input()), Some(Action::CycleTeammateForward));
    }

    #[test]
    fn enter_in_chat_pane_activates_the_highlighted_item() {
        let ev = key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(map_key(ev, chat()), Some(Action::ActivateChatItem));
    }

    #[test]
    fn question_mark_shows_chat_help() {
        let ev = key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(map_key(ev, chat()), Some(Action::ShowChatHelp));
    }

    // ── PgUp/PgDn: global chat scroll, regardless of focus ──────────────────

    #[test]
    fn page_up_scrolls_chat_when_no_pane_focused() {
        let ev = key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(map_key(ev, chat()), Some(Action::ScrollFullPageUp));
    }

    #[test]
    fn page_down_scrolls_chat_when_no_pane_focused() {
        let ev = key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(map_key(ev, chat()), Some(Action::ScrollFullPageDown));
    }

    #[test]
    fn page_up_scrolls_chat_while_input_focused() {
        // This is the reported bug: PgUp used to be swallowed as
        // `InputPageUp` (paging the empty input buffer) instead of scrolling
        // the output.
        let ev = key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(map_key(ev, input()), Some(Action::ScrollFullPageUp));
    }

    #[test]
    fn page_down_scrolls_chat_while_input_focused() {
        let ev = key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(map_key(ev, input()), Some(Action::ScrollFullPageDown));
    }

    #[test]
    fn page_up_scrolls_chat_while_queue_focused() {
        let ev = key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(map_key(ev, queue()), Some(Action::ScrollFullPageUp));
    }

    #[test]
    fn page_up_scrolls_chat_while_searching() {
        let ev = key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(
            map_key(
                ev,
                KeyContext {
                    searching: true,
                    ..input()
                }
            ),
            Some(Action::ScrollFullPageUp)
        );
    }

    #[test]
    fn alt_page_up_still_pages_input_buffer() {
        let ev = key(KeyCode::PageUp, KeyModifiers::ALT);
        assert_eq!(map_key(ev, input()), Some(Action::InputPageUp));
    }

    #[test]
    fn alt_page_down_still_pages_input_buffer() {
        let ev = key(KeyCode::PageDown, KeyModifiers::ALT);
        assert_eq!(map_key(ev, input()), Some(Action::InputPageDown));
    }

    #[test]
    fn page_up_pages_edit_buffer_while_editing_a_message() {
        // Inline-edit mode is a distinct, focused text-editing context - PgUp
        // there pages the message being edited, not the background chat.
        let ev = key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, editing(FocusPane::Chat)),
            Some(Action::InputPageUp)
        );
    }

    #[test]
    fn page_down_pages_edit_buffer_while_editing_a_message() {
        let ev = key(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, editing(FocusPane::Chat)),
            Some(Action::InputPageDown)
        );
    }
}
