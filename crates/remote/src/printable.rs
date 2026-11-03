// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for remotely driven agents whose
// own console cannot be turned against the person watching it for its
// clients. If your team needs expertise in terminal-safe agent tooling then
// you can procure our services by sending an email to
// info@swedishembedded.com.

//! Text from the server or a model, made safe to print on a terminal.
//!
//! What `sven rc` prints (a message's first line, a tool's name, an error, a
//! transcript line) comes from the server or from a model. Printed as it is,
//! an escape sequence in it is obeyed by the terminal: OSC 52 writes the
//! clipboard, cursor movement rewrites or hides the lines above (an approval
//! notice), a title sequence renames the window.

use std::borrow::Cow;

/// `text` with every control character (escape, bell, carriage return,
/// C1 controls...) shown as `\u{...}` instead of acted on. A tab and a
/// line feed are kept: they move on, never back over what was printed.
#[must_use]
pub fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_unsafe) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| {
                if is_unsafe(c) {
                    c.escape_unicode().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect(),
    )
}

fn is_unsafe(c: char) -> bool {
    c.is_control() && c != '\t' && c != '\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_escape_sequence_is_shown_never_obeyed() {
        let hostile = "ok\u{1b}]52;c;ZXZpbA==\u{7}\u{1b}[2A\rdone\u{9b}";
        let shown = printable(hostile);
        assert!(
            !shown.chars().any(|c| c.is_control() && c != '\n'),
            "{shown:?}"
        );
        assert!(shown.starts_with("ok") && shown.contains("done"));
    }

    #[test]
    fn ordinary_text_is_left_as_it_is() {
        assert!(matches!(
            printable("build\tpassed ✓\nnext line"),
            Cow::Borrowed(_)
        ));
    }
}
