// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Key-event utilities: which keys are reserved for the TUI host and how to
//! encode keys in Neovim notation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Returns `true` when a key event is reserved for sven and must NOT be
/// forwarded to the embedded Neovim instance.
pub fn is_reserved_key(event: &KeyEvent) -> bool {
    matches!(
        (event.modifiers, event.code),
        (KeyModifiers::CONTROL, KeyCode::Char('w'))  // Pane-switching prefix
        | (KeyModifiers::CONTROL, KeyCode::Char('t'))  // Pager toggle
        | (KeyModifiers::CONTROL, KeyCode::Enter)  // Submit buffer to agent
        | (KeyModifiers::NONE, KeyCode::F(1))  // Help
        | (KeyModifiers::NONE, KeyCode::F(4))  // Mode cycle
        | (KeyModifiers::NONE, KeyCode::Char('/'))  // Search (when not in nvim)
    )
}

/// Convert a `crossterm` `KeyEvent` to Neovim key notation, e.g. `"<C-u>"`.
///
/// Returns `None` for key codes that have no Neovim representation.
pub fn to_nvim_notation(event: &KeyEvent) -> Option<String> {
    let key_str = match event.code {
        KeyCode::Char(c) => {
            if event.modifiers.contains(KeyModifiers::CONTROL) {
                format!("<C-{}>", c)
            } else if event.modifiers.contains(KeyModifiers::ALT) {
                format!("<A-{}>", c)
            } else if event.modifiers.contains(KeyModifiers::SHIFT) && c.is_alphabetic() {
                c.to_uppercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyCode::Enter => {
            if event.modifiers.contains(KeyModifiers::CONTROL) {
                "<C-CR>".to_string()
            } else if event.modifiers.contains(KeyModifiers::SHIFT) {
                "<S-CR>".to_string()
            } else {
                "<CR>".to_string()
            }
        }
        KeyCode::Esc       => "<Esc>".to_string(),
        KeyCode::Backspace => "<BS>".to_string(),
        KeyCode::Delete    => "<Del>".to_string(),
        KeyCode::Tab => {
            if event.modifiers.contains(KeyModifiers::SHIFT) {
                "<S-Tab>".to_string()
            } else {
                "<Tab>".to_string()
            }
        }
        KeyCode::Up       => "<Up>".to_string(),
        KeyCode::Down     => "<Down>".to_string(),
        KeyCode::Left     => "<Left>".to_string(),
        KeyCode::Right    => "<Right>".to_string(),
        KeyCode::Home     => "<Home>".to_string(),
        KeyCode::End      => "<End>".to_string(),
        KeyCode::PageUp   => "<PageUp>".to_string(),
        KeyCode::PageDown => "<PageDown>".to_string(),
        KeyCode::F(n)     => format!("<F{}>", n),
        _                 => return None,
    };

    Some(key_str)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::*;

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    // ── is_reserved_key ───────────────────────────────────────────────────────

    #[test]
    fn pane_switch_prefix_ctrl_w_is_reserved() {
        let event = press(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(is_reserved_key(&event), "Ctrl+W must be reserved for pane-switch prefix");
    }

    #[test]
    fn ctrl_c_not_reserved() {
        let event = press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!is_reserved_key(&event));
    }

    #[test]
    fn pager_ctrl_t_is_reserved() {
        let event = press(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(is_reserved_key(&event), "Ctrl+T must be reserved for pager");
    }

    #[test]
    fn help_f1_is_reserved() {
        let event = press(KeyCode::F(1), KeyModifiers::NONE);
        assert!(is_reserved_key(&event), "F1 must be reserved for help");
    }

    #[test]
    fn mode_cycle_f4_is_reserved() {
        let event = press(KeyCode::F(4), KeyModifiers::NONE);
        assert!(is_reserved_key(&event), "F4 must be reserved for mode cycle");
    }

    #[test]
    fn vim_motion_and_editing_keys_are_not_reserved() {
        let vim_keys = ['h', 'j', 'k', 'l', 'i', 'o', 'v', 'G', 'g', 'z', 'Z', 'c', 'd', 'y', 'p'];
        for c in vim_keys {
            let event = press(KeyCode::Char(c), KeyModifiers::NONE);
            assert!(!is_reserved_key(&event), "vim key '{c}' must not be reserved");
        }
    }

    #[test]
    fn arrow_and_navigation_keys_are_not_reserved() {
        let nav_keys = [
            KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right,
            KeyCode::PageUp, KeyCode::PageDown, KeyCode::Home, KeyCode::End,
        ];
        for code in nav_keys {
            let event = press(code, KeyModifiers::NONE);
            assert!(!is_reserved_key(&event), "{code:?} must not be reserved");
        }
    }

    #[test]
    fn escape_is_not_reserved() {
        let event = press(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!is_reserved_key(&event), "Esc must not be reserved (Neovim handles it)");
    }

    // ── to_nvim_notation ──────────────────────────────────────────────────────

    #[test]
    fn plain_alphabetic_char_passes_through_unchanged() {
        let cases = [('j', "j"), ('G', "G"), ('i', "i"), ('z', "z")];
        for (c, expected) in cases {
            let result = to_nvim_notation(&press(KeyCode::Char(c), KeyModifiers::NONE));
            assert_eq!(result, Some(expected.into()), "char '{c}'");
        }
    }

    #[test]
    fn ctrl_char_encoded_as_angle_bracket_c_notation() {
        let cases = [('u', "<C-u>"), ('d', "<C-d>"), ('r', "<C-r>"), ('o', "<C-o>")];
        for (c, expected) in cases {
            let result = to_nvim_notation(&press(KeyCode::Char(c), KeyModifiers::CONTROL));
            assert_eq!(result, Some(expected.into()), "Ctrl+{c}");
        }
    }

    #[test]
    fn special_keys_encoded_with_angle_bracket_names() {
        let cases: &[(KeyCode, &str)] = &[
            (KeyCode::Esc,       "<Esc>"),
            (KeyCode::Enter,     "<CR>"),
            (KeyCode::Backspace, "<BS>"),
            (KeyCode::Delete,    "<Del>"),
            (KeyCode::Tab,       "<Tab>"),
        ];
        for (code, expected) in cases {
            let result = to_nvim_notation(&press(*code, KeyModifiers::NONE));
            assert_eq!(result, Some((*expected).into()), "{code:?}");
        }
    }

    #[test]
    fn directional_keys_encoded_with_direction_names() {
        let cases: &[(KeyCode, &str)] = &[
            (KeyCode::Up,    "<Up>"),
            (KeyCode::Down,  "<Down>"),
            (KeyCode::Left,  "<Left>"),
            (KeyCode::Right, "<Right>"),
        ];
        for (code, expected) in cases {
            let result = to_nvim_notation(&press(*code, KeyModifiers::NONE));
            assert_eq!(result, Some((*expected).into()), "{code:?}");
        }
    }

    #[test]
    fn page_and_function_keys_encoded_correctly() {
        let cases: &[(KeyCode, &str)] = &[
            (KeyCode::PageUp,   "<PageUp>"),
            (KeyCode::PageDown, "<PageDown>"),
            (KeyCode::F(1),     "<F1>"),
            (KeyCode::F(5),     "<F5>"),
            (KeyCode::F(12),    "<F12>"),
        ];
        for (code, expected) in cases {
            let result = to_nvim_notation(&press(*code, KeyModifiers::NONE));
            assert_eq!(result, Some((*expected).into()), "{code:?}");
        }
    }
}
