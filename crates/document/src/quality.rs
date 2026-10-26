// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Is a page's native text layer worth keeping?
//!
//! Scanned archives often carry a text layer from an old OCR pass that is
//! present but wrong. Presence therefore decides nothing: the layer is judged
//! by how much of it looks like words.

/// What the native text of one page amounts to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextQuality {
    /// No usable text: absent or nearly empty (a scanned page).
    Missing,
    /// Text exists but is mostly not words (a failed earlier OCR pass).
    Poor,
    /// Enough text, and nearly all of it is word-shaped.
    Good,
}

/// Fewest alphanumeric characters for a page to count as having text.
const MIN_ALNUM: usize = 60;
/// Share of word-shaped tokens a good layer reaches.
const MIN_PLAUSIBLE: f32 = 0.90;

pub fn assess(text: &str) -> TextQuality {
    let alnum = text.chars().filter(|c| c.is_alphanumeric()).count();
    if alnum < MIN_ALNUM {
        return TextQuality::Missing;
    }
    let junk = text
        .chars()
        .filter(|c| *c == '\u{FFFD}' || ('\u{E000}'..='\u{F8FF}').contains(c))
        .count();
    if junk * 50 > text.chars().count() {
        return TextQuality::Poor;
    }
    if plausible_share(text) >= MIN_PLAUSIBLE {
        TextQuality::Good
    } else {
        TextQuality::Poor
    }
}

/// Share of whitespace-separated tokens that look like words or numbers.
pub fn plausible_share(text: &str) -> f32 {
    let (mut total, mut ok) = (0u32, 0u32);
    for token in text.split_whitespace() {
        total += 1;
        if plausible_token(token) {
            ok += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        ok as f32 / total as f32
    }
}

fn plausible_token(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !c.is_alphanumeric());
    if core.is_empty() {
        // Bare punctuation: a dash or bullet, harmless.
        return token.chars().count() <= 3;
    }
    if core
        .chars()
        .any(|c| !(c.is_alphanumeric() || "'’-._/:%".contains(c)))
    {
        return false;
    }
    let parts: Vec<&str> = core
        .split(|c: char| "'’-._/:%".contains(c))
        .filter(|p| !p.is_empty())
        .collect();
    let compound = parts.len() > 1;
    parts.iter().all(|p| plausible_part(p, compound))
}

/// One alphanumeric run of a token. Technical text is full of codes
/// (`0x5006`, `AHB1ENR`) and acronyms; only runs of letters that no language
/// produces mark a broken text layer.
fn plausible_part(part: &str, in_compound: bool) -> bool {
    if part.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    let letters: Vec<char> = part.chars().collect();
    // Scripts without a vowel notion: trust them.
    if letters
        .iter()
        .any(|c| !c.is_ascii() && !is_latin_extended(*c))
    {
        return true;
    }
    if letters.len() == 1 {
        return in_compound
            || matches!(letters[0], 'a' | 'A' | 'I' | 'i' | 'O' | 'o')
            || !letters[0].is_ascii();
    }
    if letters.iter().all(|c| c.is_uppercase()) {
        return letters.len() <= 12;
    }
    if !letters.iter().any(|c| is_vowel(*c)) {
        return false;
    }
    let mut run = 0;
    for c in &letters {
        run = if is_vowel(*c) { 0 } else { run + 1 };
        if run >= 6 {
            return false;
        }
    }
    // Internal capitals ("tHe", "aBcD") mark misrecognised glyphs.
    letters.iter().skip(1).filter(|c| c.is_uppercase()).count() <= 1
}

fn is_vowel(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y') || !c.is_ascii()
}

fn is_latin_extended(c: char) -> bool {
    ('\u{00C0}'..='\u{024F}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_is_good_scanned_is_missing_garbage_is_poor() {
        let prose = "The quick brown fox jumps over the lazy dog while the committee reviews \
                     the annual budget for 2024, which grew by 3.5% compared with last year.";
        assert_eq!(assess(prose), TextQuality::Good);
        assert_eq!(assess("  \n 3 "), TextQuality::Missing);
        let garbage = "tbe qvxkr; Hnpwq jjgfd !!lKj ssrtq mNbvc ,,zxcvb xqzzk wwrtt ptrqb \
                       fgdsa ;;lkjh nbvcx qwrtp zzxcv bnmkl ,mnbv cxzlk jhgfd sapoi uytrw";
        assert_eq!(assess(garbage), TextQuality::Poor);
    }

    #[test]
    fn non_english_text_with_diacritics_is_good() {
        let pl = "Zażółć gęślą jaźń to zdanie zawierające wszystkie polskie litery, \
                  które pojawia się w wielu podręcznikach do nauki języka.";
        assert_eq!(assess(pl), TextQuality::Good);
    }
}
