// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Syntax highlighting via syntect, with OnceLock-cached SyntaxSet/ThemeSet.

use std::sync::OnceLock;

/// Per-token highlight data: text fragment + RGB color.
pub type HighlightToken = (String, u8, u8, u8);

static SYNTAX_SET: OnceLock<syntect::parsing::SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<syntect::highlighting::ThemeSet> = OnceLock::new();

fn syntax_set() -> &'static syntect::parsing::SyntaxSet {
    SYNTAX_SET.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

fn highlight_theme() -> &'static syntect::highlighting::Theme {
    let ts = THEME_SET.get_or_init(syntect::highlighting::ThemeSet::load_defaults);
    &ts.themes["base16-ocean.dark"]
}

/// Highlight `code` using syntect.  Returns one `Vec<HighlightToken>` per line.
/// Falls back to a single plain cyan-coloured line if the language is unknown.
/// Loading the SyntaxSet/ThemeSet is deferred to the first call via `OnceLock`.
pub fn highlight_code(language: &str, code: &str) -> Vec<Vec<HighlightToken>> {
    use syntect::easy::HighlightLines;
    use syntect::util::LinesWithEndings;

    let ss = syntax_set();
    let theme = highlight_theme();

    let syntax = ss
        .find_syntax_by_token(language)
        .or_else(|| ss.find_syntax_by_extension(language))
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut h = HighlightLines::new(syntax, theme);
    let mut result: Vec<Vec<HighlightToken>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = match h.highlight_line(line, ss) {
            Ok(r) => r,
            Err(_) => {
                result.push(vec![(
                    line.trim_end_matches('\n').to_string(),
                    0xa5,
                    0xd6,
                    0xff,
                )]);
                continue;
            }
        };

        let tokens: Vec<HighlightToken> = ranges
            .iter()
            .filter_map(|(style, text)| {
                let t = text.trim_end_matches('\n');
                if t.is_empty() {
                    None
                } else {
                    Some((
                        t.to_string(),
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                    ))
                }
            })
            .collect();

        result.push(tokens);
    }

    result
}
