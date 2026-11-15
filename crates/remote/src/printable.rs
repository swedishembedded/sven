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

/// The arguments that say what a call was asked, shown first when a call
/// has them.
const TELLING: [&str; 10] = [
    "model",
    "operation",
    "path",
    "file_path",
    "command",
    "url",
    "query",
    "name",
    "pattern",
    "description",
];

/// The most arguments a call's summary shows.
const SHOWN_ARGS: usize = 4;

/// The longest value a call's summary shows of one argument.
const ARG_CHARS: usize = 60;

/// A tool call as a log line names it: its name and the few arguments that
/// say what it was asked (those in [`TELLING`] first, then the others in
/// order), each a short line made safe to print. Nested values are left
/// out: a log line names a call, it does not repeat it.
#[must_use]
pub fn call_summary(name: &str, args: &serde_json::Value) -> String {
    let Some(fields) = args.as_object() else {
        return printable(name).into_owned();
    };
    let mut keys: Vec<&String> = TELLING
        .iter()
        .filter_map(|k| fields.get_key_value(*k).map(|(k, _)| k))
        .collect();
    keys.extend(fields.keys().filter(|k| !TELLING.contains(&k.as_str())));
    let shown: Vec<String> = keys
        .into_iter()
        .filter_map(|key| {
            let value = match &fields[key] {
                serde_json::Value::String(text) => {
                    text.lines().next().unwrap_or_default().to_string()
                }
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            let value: String = if value.chars().count() > ARG_CHARS {
                format!("{}…", value.chars().take(ARG_CHARS).collect::<String>())
            } else {
                value
            };
            Some(format!("{}: {}", printable(key), printable(&value)))
        })
        .take(SHOWN_ARGS)
        .collect();
    if shown.is_empty() {
        return printable(name).into_owned();
    }
    format!("{} {{{}}}", printable(name), shown.join(", "))
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
