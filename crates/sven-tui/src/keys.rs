// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// All logical actions the TUI can perform, independent of key binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // Navigation
    FocusInput,
    /// First key of the Ctrl+w nav chord (vim-style window navigation).
    /// The App will watch for a follow-up key to decide the target pane.
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
    InputBackspace,
    InputDelete,
    InputMoveCursorLeft,
    InputMoveCursorRight,
    InputMoveWordLeft,
    InputMoveWordRight,
    InputMoveLineStart,
    InputMoveLineEnd,
    InputMoveLineUp,
    InputMoveLineDown,
    InputPageUp,
    InputPageDown,
    InputDeleteToEnd,
    InputDeleteToStart,
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
    /// Submit the selected queued message immediately, even if the agent is
    /// busy.  The current run is aborted (partial text is preserved) and the
    /// selected message is sent as the next user turn.
    ForceSubmitQueuedMessage,
    /// Submit the selected queued message when the agent is idle (manual
    /// dequeue after an abort).  Clears `abort_pending`.
    QueueSubmitSelected,

    // Buffer submit (Neovim integration)
    SubmitBufferToAgent,

    // Completion overlay
    /// Select the next completion item (Tab / Down when overlay visible).
    CompletionNext,
    /// Select the previous completion item (Shift+Tab / Up when overlay visible).
    CompletionPrev,
    /// Accept the currently highlighted completion item (Enter when overlay visible).
    CompletionSelect,
    /// Dismiss the completion overlay without selecting (Esc when overlay visible).
    CompletionCancel,

    // App
    Help,
    OpenPager,
}

/// Map a raw key event to an [`Action`], depending on which pane has focus.
///
/// `pending_nav` — true when a Ctrl+w prefix has been received but not yet
/// resolved.  In that state only j/k (and Esc to cancel) are meaningful.
/// `in_edit_mode` — true when editing a queued message; Enter/Esc confirm/cancel, rest goes to input.
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
    // After a Ctrl+w prefix, we only look for j/k to pick the next pane.
    // The direction is context-aware: if a queue panel is visible it will be
    // included in the cycle (Input ↔ Queue ↔ Chat).
    // Any other key cancels the prefix (returning None causes the App to clear
    // the flag without acting).
    if pending_nav {
        return match event.code {
            KeyCode::Char('k') | KeyCode::Up => Some(Action::NavUp),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::NavDown),
            _ => None, // cancel without action
        };
    }

    if in_search {
        return map_search_key(event);
    }

    // ── Edit message mode: confirm, cancel, or route to input ─────────────────
    if in_edit_mode {
        return match event.code {
            KeyCode::Enter if shift || ctrl || alt => Some(Action::InputNewline),
            KeyCode::Enter => Some(Action::EditMessageConfirm),
            KeyCode::Char(' ') if shift => Some(Action::InputNewline), // Shift+Enter decoded as space
            KeyCode::Char('j' | 'm') if ctrl => Some(Action::InputNewline),
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
            // Enter — force-submit: abort current run (if any) and send the selected
            // message immediately.  When the agent is idle this is equivalent to a
            // normal submit; when busy it aborts the current turn first.
            KeyCode::Enter => Some(Action::ForceSubmitQueuedMessage),
            KeyCode::Char('d') | KeyCode::Delete => Some(Action::DeleteQueuedMessage),
            // 'f' — force-submit (same as Enter, kept for muscle memory).
            KeyCode::Char('f') => Some(Action::ForceSubmitQueuedMessage),
            // 's' — submit selected message only when the agent is idle (manual
            // dequeue without aborting the running turn).
            KeyCode::Char('s') => Some(Action::QueueSubmitSelected),
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::FocusInput),
            KeyCode::Char('w') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::NavPrefix)
            }
            _ => None,
        };
    }

    match event.code {
        // ── Input-pane overrides come FIRST so they shadow global bindings ────
        // Ctrl+c in input — interrupt agent
        KeyCode::Char('c') if ctrl && in_input => Some(Action::InterruptAgent),
        // Ctrl+u — delete to line start
        KeyCode::Char('u') if ctrl && in_input => Some(Action::InputDeleteToStart),
        // Ctrl+k — delete to line end
        KeyCode::Char('k') if ctrl && in_input => Some(Action::InputDeleteToEnd),

        // ── Global bindings ───────────────────────────────────────────────────
        // Quit is via :q/:qa in Neovim chat view and /quit in input pane (no Ctrl+C/Ctrl+Q)

        // Ctrl+w → start the nav-prefix chord (works from any pane)
        KeyCode::Char('w') if ctrl => Some(Action::NavPrefix),

        // Global cycle / help / pager
        KeyCode::F(1) => Some(Action::Help),
        KeyCode::F(4) => Some(Action::CycleMode),
        KeyCode::Char('t') if ctrl => Some(Action::OpenPager),

        // ── Rest of input pane ────────────────────────────────────────────────
        // Tab / Shift+Tab cycle completions when the input buffer starts with '/'
        // The App decides at dispatch time whether the overlay is active.
        KeyCode::Tab if in_input && !shift => Some(Action::CompletionNext),
        KeyCode::BackTab if in_input => Some(Action::CompletionPrev),
        KeyCode::Enter if in_input && !shift && !ctrl && !alt => Some(Action::Submit),
        KeyCode::Enter if in_input && shift => Some(Action::InputNewline),
        KeyCode::Enter if in_input && ctrl => Some(Action::InputNewline),
        KeyCode::Enter if in_input && alt => Some(Action::InputNewline),
        // Some terminals send Shift+Enter as KeyCode::Char(' ') with Shift; treat as newline
        KeyCode::Char(' ') if in_input && shift => Some(Action::InputNewline),
        // Ctrl+J / Ctrl+M insert newline (fallback when terminal doesn't report Shift+Enter)
        KeyCode::Char('j' | 'm') if ctrl && in_input => Some(Action::InputNewline),
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
        // Printable characters — only when no ctrl/alt modifier
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

        // Edit / delete focused segment (chat pane).
        // 'e' / 'd' work in no-nvim mode; F2 / F8 work in both modes
        // (F2 and F8 are intercepted as reserved keys before nvim receives them).
        KeyCode::Char('e') if !in_input && plain => Some(Action::EditMessageAtCursor),
        KeyCode::F(2) if !in_input => Some(Action::EditMessageAtCursor),
        // 'd' / F8 truncate history from the focused segment onward.
        KeyCode::Char('d') if !in_input && plain => Some(Action::DeleteChatSegment),
        KeyCode::F(8) if !in_input => Some(Action::DeleteChatSegment),
        // 'x' removes only the focused segment (ToolCall/Result pair too).
        KeyCode::Char('x') if !in_input && plain => Some(Action::RemoveChatSegment),
        // 'r' reruns from the focused segment (truncate + resubmit to agent).
        KeyCode::Char('r') if !in_input && plain => Some(Action::RerunFromSegment),
        // Focus queue panel when in chat pane
        KeyCode::Char('q') if !in_input && plain => Some(Action::FocusQueue),

        // Submit buffer to agent (Ctrl+Enter from chat pane with Neovim)
        KeyCode::Enter if !in_input && ctrl => Some(Action::SubmitBufferToAgent),

        _ => None,
    }
}

pub(crate) fn map_search_key(event: KeyEvent) -> Option<Action> {
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);

    match event.code {
        KeyCode::Esc => Some(Action::SearchClose),
        KeyCode::Enter => Some(Action::SearchClose), // Close search, stay on current match
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

    // ── Ctrl+w chord ─────────────────────────────────────────────────────────

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
        assert_eq!(
            map_key(ev, false, true, true, false, false),
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
    fn pending_nav_up_returns_nav_up() {
        let ev = key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, false, true, false, false),
            Some(Action::NavUp)
        );
    }

    #[test]
    fn pending_nav_other_key_cancels() {
        let ev = plain_key('x');
        assert_eq!(map_key(ev, false, false, true, false, false), None);
    }

    // ── Ctrl modifier should NOT type a character ─────────────────────────────

    #[test]
    fn ctrl_w_in_input_does_not_type_w() {
        let ev = ctrl_key('w');
        // Should be NavPrefix, not InputChar('w')
        let action = map_key(ev, false, true, false, false, false);
        assert_ne!(action, Some(Action::InputChar('w')));
        assert_eq!(action, Some(Action::NavPrefix));
    }

    #[test]
    fn ctrl_x_unbound_does_not_type_x() {
        let ev = ctrl_key('x');
        assert_eq!(map_key(ev, false, true, false, false, false), None);
    }

    #[test]
    fn alt_char_in_input_does_not_type() {
        let ev = key(KeyCode::Char('a'), KeyModifiers::ALT);
        assert_eq!(map_key(ev, false, true, false, false, false), None);
    }

    // ── Normal typing ─────────────────────────────────────────────────────────

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
        // 'x' is bound to RemoveChatSegment when not in the input box.
        let ev = plain_key('x');
        assert_eq!(
            map_key(ev, false, false, false, false, false),
            Some(Action::RemoveChatSegment),
        );
    }

    // ── Ctrl+k in input deletes to end ────────────────────────────────────────

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
        // Ctrl+k is no longer a pane-switch key; in chat pane it's unbound
        let ev = ctrl_key('k');
        assert_eq!(map_key(ev, false, false, false, false, false), None);
    }

    // ── Global quit ───────────────────────────────────────────────────────────

    #[test]
    fn ctrl_c_outside_input_not_reserved() {
        // Quit is via :q/:qa in chat and /quit in input; Ctrl+C outside input is not bound (forwarded to Neovim)
        let ev = ctrl_key('c');
        assert_eq!(map_key(ev, false, false, false, false, false), None);
    }

    #[test]
    fn ctrl_c_interrupts_inside_input() {
        // In input pane Ctrl+c is InterruptAgent
        let ev = ctrl_key('c');
        assert_eq!(
            map_key(ev, false, true, false, false, false),
            Some(Action::InterruptAgent)
        );
    }

    // ── Chat scrolling ────────────────────────────────────────────────────────

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

    // ── Edit message mode ────────────────────────────────────────────────────

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
}
