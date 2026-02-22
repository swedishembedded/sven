// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// All logical actions the TUI can perform, independent of key binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // Navigation
    FocusChat,
    FocusInput,
    /// First key of the Ctrl+w nav chord (vim-style window navigation).
    /// The App will watch for a follow-up key to decide the target pane.
    NavPrefix,

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

    // Buffer submit (Neovim integration)
    SubmitBufferToAgent,

    // App
    Help,
    OpenPager,
}

/// Map a raw key event to an [`Action`], depending on which pane has focus.
///
/// `pending_nav` — true when a Ctrl+w prefix has been received but not yet
/// resolved.  In that state only j/k (and Esc to cancel) are meaningful.
/// `in_edit_mode` — true when editing a message; Enter/Esc confirm/cancel, rest goes to input.
pub fn map_key(
    event: KeyEvent,
    in_search: bool,
    in_input: bool,
    pending_nav: bool,
    in_edit_mode: bool,
) -> Option<Action> {
    let ctrl  = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt   = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    // "plain" = no modifier that would make a char a control sequence
    let plain = !ctrl && !alt;

    // ── Pending Ctrl+w chord ──────────────────────────────────────────────────
    // After a Ctrl+w prefix, we only look for j/k/h/l to pick a pane.
    // Any other key cancels the prefix (returning None causes the App to clear
    // the flag without acting).
    if pending_nav {
        return match event.code {
            KeyCode::Char('k') | KeyCode::Up   => Some(Action::FocusChat),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::FocusInput),
            _ => None, // cancel without action
        };
    }

    if in_search {
        return map_search_key(event);
    }

    // ── Edit message mode: confirm, cancel, or route to input ─────────────────
    if in_edit_mode {
        return match event.code {
            KeyCode::Enter if shift => Some(Action::InputNewline),
            KeyCode::Enter => Some(Action::EditMessageConfirm),
            KeyCode::Char(' ') if shift => Some(Action::InputNewline), // Shift+Enter decoded as space
            KeyCode::Char('j' | 'm') if ctrl => Some(Action::InputNewline),
            KeyCode::Esc => Some(Action::EditMessageCancel),
            KeyCode::Backspace => Some(Action::InputBackspace),
            KeyCode::Delete => Some(Action::InputDelete),
            KeyCode::Left  if ctrl => Some(Action::InputMoveWordLeft),
            KeyCode::Right if ctrl => Some(Action::InputMoveWordRight),
            KeyCode::Left  => Some(Action::InputMoveCursorLeft),
            KeyCode::Right => Some(Action::InputMoveCursorRight),
            KeyCode::Home  => Some(Action::InputMoveLineStart),
            KeyCode::End   => Some(Action::InputMoveLineEnd),
            KeyCode::Char('u') if ctrl => Some(Action::InputDeleteToStart),
            KeyCode::Char('k') if ctrl => Some(Action::InputDeleteToEnd),
            KeyCode::Char(c) if plain => Some(Action::InputChar(c)),
            _ => None,
        };
    }

    match event.code {
        // ── Input-pane overrides come FIRST so they shadow global bindings ────
        // Ctrl+c in input — interrupt agent
        KeyCode::Char('c') if ctrl && in_input  => Some(Action::InterruptAgent),
        // Ctrl+u — delete to line start
        KeyCode::Char('u') if ctrl && in_input  => Some(Action::InputDeleteToStart),
        // Ctrl+k — delete to line end
        KeyCode::Char('k') if ctrl && in_input  => Some(Action::InputDeleteToEnd),

        // ── Global bindings ───────────────────────────────────────────────────
        // Quit is via :q/:qa in Neovim chat view and /quit in input pane (no Ctrl+C/Ctrl+Q)

        // Ctrl+w → start the nav-prefix chord (works from any pane)
        KeyCode::Char('w') if ctrl => Some(Action::NavPrefix),

        // Global cycle / help / pager
        KeyCode::F(1) => Some(Action::Help),
        KeyCode::F(4) => Some(Action::CycleMode),
        KeyCode::Char('t') if ctrl => Some(Action::OpenPager),

        // ── Rest of input pane ────────────────────────────────────────────────
        KeyCode::Enter    if in_input && !shift => Some(Action::Submit),
        KeyCode::Enter    if in_input &&  shift => Some(Action::InputNewline),
        // Some terminals send Shift+Enter as KeyCode::Char(' ') with Shift; treat as newline
        KeyCode::Char(' ') if in_input &&  shift => Some(Action::InputNewline),
        // Ctrl+J / Ctrl+M insert newline (fallback when terminal doesn't report Shift+Enter)
        KeyCode::Char('j' | 'm') if ctrl && in_input => Some(Action::InputNewline),
        KeyCode::Backspace if in_input          => Some(Action::InputBackspace),
        KeyCode::Delete    if in_input          => Some(Action::InputDelete),
        KeyCode::Left  if in_input && ctrl      => Some(Action::InputMoveWordLeft),
        KeyCode::Right if in_input && ctrl      => Some(Action::InputMoveWordRight),
        KeyCode::Left  if in_input              => Some(Action::InputMoveCursorLeft),
        KeyCode::Right if in_input              => Some(Action::InputMoveCursorRight),
        KeyCode::Home  if in_input              => Some(Action::InputMoveLineStart),
        KeyCode::End   if in_input              => Some(Action::InputMoveLineEnd),
        // Printable characters — only when no ctrl/alt modifier
        KeyCode::Char(c) if in_input && plain   => Some(Action::InputChar(c)),

        // ── Chat pane ─────────────────────────────────────────────────────────
        KeyCode::Up   | KeyCode::Char('k') if !in_input && plain => Some(Action::ScrollUp),
        KeyCode::Down | KeyCode::Char('j') if !in_input && plain => Some(Action::ScrollDown),
        KeyCode::Char('K') if !in_input => Some(Action::ScrollUp),
        KeyCode::Char('J') if !in_input => Some(Action::ScrollDown),
        KeyCode::Char('u') if ctrl && !in_input => Some(Action::ScrollPageUp),
        KeyCode::Char('d') if ctrl && !in_input => Some(Action::ScrollPageDown),
        KeyCode::Char('g') if !in_input && plain => Some(Action::ScrollTop),
        KeyCode::Char('G') if !in_input          => Some(Action::ScrollBottom),

        // Search
        KeyCode::Char('/') if !in_input && plain => Some(Action::SearchOpen),
        KeyCode::Char('n') if !in_input && plain => Some(Action::SearchNextMatch),
        KeyCode::Char('N') if !in_input          => Some(Action::SearchPrevMatch),

        // Edit message at cursor (chat pane)
        KeyCode::Char('e') if !in_input && plain => Some(Action::EditMessageAtCursor),

        // Submit buffer to agent (Ctrl+Enter from chat pane with Neovim)
        KeyCode::Enter if !in_input && ctrl => Some(Action::SubmitBufferToAgent),

        _ => None,
    }
}

pub(crate) fn map_search_key(event: KeyEvent) -> Option<Action> {
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    
    match event.code {
        KeyCode::Esc       => Some(Action::SearchClose),
        KeyCode::Enter     => Some(Action::SearchClose),  // Close search, stay on current match
        KeyCode::Backspace => Some(Action::SearchBackspace),
        KeyCode::Char('n') if !shift => Some(Action::SearchNextMatch),
        KeyCode::Char('N') | KeyCode::Char('n') if shift => Some(Action::SearchPrevMatch),
        KeyCode::Char(c)   => Some(Action::SearchInput(c)),
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

    fn plain_key(c: char) -> KeyEvent { key(KeyCode::Char(c), KeyModifiers::NONE) }
    fn ctrl_key(c: char)  -> KeyEvent { key(KeyCode::Char(c), KeyModifiers::CONTROL) }

    // ── Ctrl+w chord ─────────────────────────────────────────────────────────

    #[test]
    fn ctrl_w_returns_nav_prefix() {
        let ev = ctrl_key('w');
        assert_eq!(map_key(ev, false, false, false, false), Some(Action::NavPrefix));
        assert_eq!(map_key(ev, false, true,  false, false), Some(Action::NavPrefix));
    }

    #[test]
    fn pending_nav_k_focuses_chat() {
        let ev = plain_key('k');
        assert_eq!(map_key(ev, false, false, true, false), Some(Action::FocusChat));
        assert_eq!(map_key(ev, false, true,  true, false), Some(Action::FocusChat));
    }

    #[test]
    fn pending_nav_j_focuses_input() {
        let ev = plain_key('j');
        assert_eq!(map_key(ev, false, false, true, false), Some(Action::FocusInput));
    }

    #[test]
    fn pending_nav_up_focuses_chat() {
        let ev = key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(map_key(ev, false, false, true, false), Some(Action::FocusChat));
    }

    #[test]
    fn pending_nav_other_key_cancels() {
        let ev = plain_key('x');
        assert_eq!(map_key(ev, false, false, true, false), None);
    }

    // ── Ctrl modifier should NOT type a character ─────────────────────────────

    #[test]
    fn ctrl_w_in_input_does_not_type_w() {
        let ev = ctrl_key('w');
        // Should be NavPrefix, not InputChar('w')
        let action = map_key(ev, false, true, false, false);
        assert_ne!(action, Some(Action::InputChar('w')));
        assert_eq!(action, Some(Action::NavPrefix));
    }

    #[test]
    fn ctrl_x_unbound_does_not_type_x() {
        let ev = ctrl_key('x');
        assert_eq!(map_key(ev, false, true, false, false), None);
    }

    #[test]
    fn alt_char_in_input_does_not_type() {
        let ev = key(KeyCode::Char('a'), KeyModifiers::ALT);
        assert_eq!(map_key(ev, false, true, false, false), None);
    }

    // ── Normal typing ─────────────────────────────────────────────────────────

    #[test]
    fn plain_char_in_input_types() {
        let ev = plain_key('h');
        assert_eq!(map_key(ev, false, true, false, false), Some(Action::InputChar('h')));
    }

    #[test]
    fn plain_char_not_in_input_does_not_type() {
        let ev = plain_key('x');
        assert_eq!(map_key(ev, false, false, false, false), None);
    }

    // ── Ctrl+k in input deletes to end ────────────────────────────────────────

    #[test]
    fn ctrl_k_in_input_deletes_to_end() {
        let ev = ctrl_key('k');
        assert_eq!(map_key(ev, false, true, false, false), Some(Action::InputDeleteToEnd));
    }

    #[test]
    fn ctrl_k_in_chat_does_not_fire() {
        // Ctrl+k is no longer a pane-switch key; in chat pane it's unbound
        let ev = ctrl_key('k');
        assert_eq!(map_key(ev, false, false, false, false), None);
    }

    // ── Global quit ───────────────────────────────────────────────────────────

    #[test]
    fn ctrl_c_outside_input_not_reserved() {
        // Quit is via :q/:qa in chat and /quit in input; Ctrl+C outside input is not bound (forwarded to Neovim)
        let ev = ctrl_key('c');
        assert_eq!(map_key(ev, false, false, false, false), None);
    }

    #[test]
    fn ctrl_c_interrupts_inside_input() {
        // In input pane Ctrl+c is InterruptAgent
        let ev = ctrl_key('c');
        assert_eq!(map_key(ev, false, true, false, false), Some(Action::InterruptAgent));
    }

    // ── Chat scrolling ────────────────────────────────────────────────────────

    #[test]
    fn j_in_chat_scrolls_down() {
        let ev = plain_key('j');
        assert_eq!(map_key(ev, false, false, false, false), Some(Action::ScrollDown));
    }

    #[test]
    fn ctrl_u_in_chat_page_up() {
        let ev = ctrl_key('u');
        assert_eq!(map_key(ev, false, false, false, false), Some(Action::ScrollPageUp));
    }

    // ── Edit message mode ────────────────────────────────────────────────────

    #[test]
    fn e_in_chat_opens_edit() {
        let ev = plain_key('e');
        assert_eq!(
            map_key(ev, false, false, false, false),
            Some(Action::EditMessageAtCursor)
        );
    }

    #[test]
    fn edit_mode_enter_confirms() {
        let ev = key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, true, false, true),
            Some(Action::EditMessageConfirm)
        );
    }

    #[test]
    fn edit_mode_esc_cancels() {
        let ev = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            map_key(ev, false, true, false, true),
            Some(Action::EditMessageCancel)
        );
    }

    #[test]
    fn edit_mode_char_goes_to_input() {
        let ev = plain_key('x');
        assert_eq!(
            map_key(ev, false, true, false, true),
            Some(Action::InputChar('x'))
        );
    }
}
