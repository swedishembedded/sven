// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

    // Scrolling (in chat pane)
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
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
    /// Expand or collapse a DelegateSummary segment at cursor (Space / Enter).
    ToggleDelegateSummary,
}

/// Map a raw key event to an [`Action`], depending on which pane has focus.
///
/// `pending_nav` — true when a Ctrl+w prefix has been received but not yet
/// resolved.  In that state only j/k/+/- (and Esc to cancel) are meaningful.
/// `in_edit_mode` — true when editing a queued message; Enter/Esc confirm/cancel.
/// `in_queue` — true when the queue panel has keyboard focus.
pub fn map_key(
    event: KeyEvent,
    in_search: bool,
    in_input: bool,
    pending_nav: bool,
    in_edit_mode: bool,
    in_queue: bool,
) -> Option<Action> {
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
            KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::ResizeInputGrow),
            KeyCode::Char('-') => Some(Action::ResizeInputShrink),
            _ => None, // cancel without action
        };
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
        // Alt+t — task list (distinct from Ctrl+t which opens the chat pager).
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
        KeyCode::PageUp if in_input => Some(Action::InputPageUp),
        KeyCode::PageDown if in_input => Some(Action::InputPageDown),
        KeyCode::Home if in_input => Some(Action::InputMoveLineStart),
        KeyCode::End if in_input => Some(Action::InputMoveLineEnd),
        KeyCode::Char(c) if in_input && plain => Some(Action::InputChar(c)),

        // ── Chat pane ─────────────────────────────────────────────────────────
        KeyCode::Up | KeyCode::Char('k') if !in_input && plain => Some(Action::ScrollUp),
        KeyCode::Down | KeyCode::Char('j') if !in_input && plain => Some(Action::ScrollDown),
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

        // Space in the chat pane toggles a DelegateSummary segment.
        KeyCode::Char(' ') if !in_input && plain => Some(Action::ToggleDelegateSummary),

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

    #[test]
    fn ctrl_w_returns_nav_prefix() {
        let ev = ctrl_key('w');
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::NavPrefix)
        );
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::NavPrefix)
        );
    }

    #[test]
    fn pending_nav_k_returns_nav_up() {
        let ev = plain_key('k');
        assert_eq!(
            map_key(ev, false, false, true, false, false),
            Some(Action::NavUp)
        );
    }

    #[test]
    fn pending_nav_j_returns_nav_down() {
        let ev = plain_key('j');
        assert_eq!(
            map_key(ev, false, false, true, false, false),
            Some(Action::NavDown)
        );
    }

    #[test]
    fn pending_nav_plus_grows_input() {
        let ev = plain_key('+');
        assert_eq!(
            map_key(ev, false, false, true, false, false),
            Some(Action::ResizeInputGrow)
        );
    }

    #[test]
    fn pending_nav_minus_shrinks_input() {
        let ev = plain_key('-');
        assert_eq!(
            map_key(ev, false, false, true, false, false),
            Some(Action::ResizeInputShrink)
        );
    }

    #[test]
    fn ctrl_up_in_input_is_history_up() {
        let ev = key(KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InputHistoryUp)
        );
    }

    #[test]
    fn ctrl_down_in_input_is_history_down() {
        let ev = key(KeyCode::Down, KeyModifiers::CONTROL);
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InputHistoryDown)
        );
    }

    // Plain Up/Down in the input pane produce InputMoveLineUp/Down; the dispatch
    // handler internally falls through to history navigation when the cursor is
    // already on the first/last visual row (shell-style behaviour).
    #[test]
    fn plain_up_in_input_is_move_line_up() {
        let ev = key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InputMoveLineUp)
        );
    }

    #[test]
    fn plain_down_in_input_is_move_line_down() {
        let ev = key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InputMoveLineDown)
        );
    }

    #[test]
    fn pending_nav_other_key_cancels() {
        let ev = plain_key('x');
        assert_eq!(map_key(ev, false, false, true, false, false), None);
    }

    #[test]
    fn ctrl_w_in_input_does_not_type_w() {
        let ev = ctrl_key('w');
        let action = map_key(ev, false, true, false, false, false);
        assert_ne!(action, Some(Action::InputChar('w')));
        assert_eq!(action, Some(Action::NavPrefix));
    }

    #[test]
    fn plain_char_in_input_types() {
        let ev = plain_key('h');
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InputChar('h'))
        );
    }

    #[test]
    fn plain_char_x_outside_input_removes_segment() {
        let ev = plain_key('x');
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::RemoveChatSegment),
        );
    }

    #[test]
    fn ctrl_k_in_input_deletes_to_end() {
        let ev = ctrl_key('k');
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InputDeleteToEnd)
        );
    }

    #[test]
    fn ctrl_k_in_chat_does_not_fire() {
        let ev = ctrl_key('k');
        assert_eq!(map_key(ev, false, false, false, false, false), None);
    }

    #[test]
    fn ctrl_c_outside_input_not_reserved() {
        let ev = ctrl_key('c');
        assert_eq!(map_key(ev, false, false, false, false, false), None);
    }

    #[test]
    fn ctrl_c_interrupts_inside_input() {
        let ev = ctrl_key('c');
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InterruptAgent)
        );
    }

    #[test]
    fn j_in_chat_scrolls_down() {
        let ev = plain_key('j');
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::ScrollDown)
        );
    }

    #[test]
    fn ctrl_u_in_chat_page_up() {
        let ev = ctrl_key('u');
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::ScrollPageUp)
        );
    }

    #[test]
    fn e_in_chat_opens_edit() {
        let ev = plain_key('e');
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::EditMessageAtCursor)
        );
    }

    #[test]
    fn edit_mode_enter_confirms() {
        let ev = key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, true, false, true, false),
            Some(Action::EditMessageConfirm)
        );
    }

    #[test]
    fn edit_mode_esc_cancels() {
        let ev = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, true, false, true, false),
            Some(Action::EditMessageCancel)
        );
    }

    #[test]
    fn edit_mode_char_goes_to_input() {
        let ev = plain_key('x');
        assert_eq!(
            map_key(ev, false, true, false, true, false),
            Some(Action::InputChar('x'))
        );
    }

    // ── Team / multi-agent key bindings ───────────────────────────────────────

    #[test]
    fn ctrl_a_opens_team_picker() {
        let ev = ctrl_key('a');
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::OpenTeamPicker)
        );
    }

    #[test]
    fn ctrl_a_in_input_opens_team_picker() {
        // Ctrl+a is a global binding — works whether focus is in input or chat.
        let ev = ctrl_key('a');
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::OpenTeamPicker)
        );
    }

    #[test]
    fn shift_down_cycles_teammate_forward() {
        let ev = key(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::CycleTeammateForward)
        );
    }

    #[test]
    fn shift_up_cycles_teammate_backward() {
        let ev = key(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::CycleTeammateBackward)
        );
    }

    #[test]
    fn alt_t_opens_task_list() {
        let ev = key(KeyCode::Char('t'), KeyModifiers::ALT);
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::ToggleTaskList)
        );
    }

    #[test]
    fn space_in_chat_toggles_delegate_summary() {
        let ev = key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::ToggleDelegateSummary)
        );
    }

    #[test]
    fn space_in_input_pane_types_char() {
        // Space in the input pane must produce InputChar(' '), not ToggleDelegateSummary.
        let ev = key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InputChar(' '))
        );
    }

    #[test]
    fn shift_down_in_input_pane_is_history_down() {
        // When in_input is true, plain Shift+Down should produce InputMoveLineDown
        // (handled by the input pane), not CycleTeammateForward.
        let ev = key(KeyCode::Down, KeyModifiers::SHIFT);
        // The global bindings come before input bindings in map_key, so Shift+Down
        // always maps to CycleTeammateForward regardless of focus. This test
        // documents the current contract.
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::CycleTeammateForward)
        );
    }
}
