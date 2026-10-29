// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working reliably on local
// reasoning models for its clients. If your team needs expertise in running
// agents on local models then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Detects a reasoning stream that has fallen into a loop.
//!
//! A model that repeats itself while thinking does not make progress, however
//! many tokens it spends. A token count cannot tell that apart from a long
//! derivation, and a cap low enough to catch loops cuts honest reasoning off
//! (a tenth of a 16k window ended an agent's run mid-thought). What marks a
//! loop is the repetition: the reasoning ends in the same block written
//! several times back to back. A stretch that only recurs (code quoted again
//! to compare it, with different reasoning in between) is progress.

/// Reasoning kept for comparison. A loop whose period is more than a third of
/// this is not seen until it has run longer.
const WINDOW_CHARS: usize = 16_384;
/// Length of the most recent stretch looked for in the window; long enough
/// that natural text does not repeat it by chance.
const PROBE_CHARS: usize = 400;
/// Back-to-back copies of the repeating block, the last one included, that
/// make a loop.
const REPEATS: usize = 3;
/// New characters between two looks, so the check costs little per delta.
const CHECK_EVERY: usize = 512;

/// Watches a stream of reasoning text for repetition.
#[derive(Debug, Default)]
pub struct RepetitionWatch {
    recent: String,
    unchecked: usize,
}

impl RepetitionWatch {
    /// Feeds the next piece of reasoning. Returns how many times the latest
    /// stretch now occurs when that is a loop.
    pub fn feed(&mut self, delta: &str) -> Option<usize> {
        self.recent.push_str(delta);
        self.unchecked += delta.len();
        if self.recent.len() > 2 * WINDOW_CHARS {
            let cut = self
                .recent
                .ceil_char_boundary(self.recent.len() - WINDOW_CHARS);
            self.recent.drain(..cut);
        }
        if self.unchecked < CHECK_EVERY || self.recent.len() < PROBE_CHARS * REPEATS {
            return None;
        }
        self.unchecked = 0;
        let from = self
            .recent
            .ceil_char_boundary(self.recent.len() - PROBE_CHARS);
        let probe = &self.recent[from..];
        // Blank or near-blank text repeats by nature; it is not a loop.
        if probe.chars().filter(|c| !c.is_whitespace()).count() < PROBE_CHARS / 4 {
            return None;
        }
        // The block is what lies between the probe's previous occurrence and
        // this one; a loop is that block, back to back, REPEATS times.
        let (previous, _) = self.recent.rmatch_indices(probe).nth(1)?;
        let period = from - previous;
        let text = self.recent.as_bytes();
        let span = period * REPEATS;
        if span > text.len() {
            return None;
        }
        let tail = &text[text.len() - span..];
        let periodic = (period..span).all(|i| tail[i] == tail[i - period]);
        periodic.then(|| tail.len() / period)
    }

    /// Forgets the stretch so far: the model made progress.
    pub fn reset(&mut self) {
        self.recent.clear();
        self.unchecked = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reasoning that never repeats a stretch: pseudo-random words.
    fn derivation(chars: usize) -> String {
        derivation_seeded(chars, 1)
    }

    fn derivation_seeded(chars: usize, seed: u64) -> String {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut text = String::new();
        while text.len() < chars {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            text.push_str(&format!("w{:x} ", (state >> 33) % 5000));
        }
        text
    }

    fn first_detection(text: &str) -> Option<usize> {
        let mut watch = RepetitionWatch::default();
        let mut at = 0;
        while at < text.len() {
            let end = text.ceil_char_boundary((at + 40).min(text.len()));
            if watch.feed(&text[at..end]).is_some() {
                return Some(end);
            }
            at = end;
        }
        None
    }

    #[test]
    fn a_long_derivation_that_does_not_repeat_passes() {
        // 400k characters: some 100k tokens of reasoning.
        assert_eq!(first_detection(&derivation(400_000)), None);
    }

    #[test]
    fn short_repeated_phrases_inside_real_reasoning_pass() {
        let mut text = derivation(3_000);
        for i in 0..30 {
            text.push_str("Wait, let me re-check that. ");
            text.push_str(&derivation_seeded(300, i + 2));
        }
        assert_eq!(first_detection(&text), None);
    }

    #[test]
    fn code_quoted_again_between_different_reasoning_passes() {
        // Recorded false positive: a model compared versions of a function by
        // quoting it three times, reasoning differently in between. The
        // stretch recurs, but the reasoning moves on: not a loop.
        let code = derivation_seeded(900, 99);
        let mut text = derivation(2_000);
        for i in 0..3 {
            text.push_str(&code);
            text.push_str(&derivation_seeded(600, i + 10));
        }
        text.push_str(&code);
        assert_eq!(first_detection(&text), None);
    }

    #[test]
    fn a_paragraph_repeated_over_and_over_is_stopped() {
        let paragraph = derivation(700);
        let mut text = derivation(5_000);
        for _ in 0..6 {
            text.push_str(&paragraph);
        }
        let at = first_detection(&text).expect("a loop is detected");
        // Stopped by the third round, long before the stream ends.
        assert!(at < 5_000 + 3 * 700 + 1_000, "detected only at {at}");
    }

    #[test]
    fn a_single_phrase_repeated_endlessly_is_stopped() {
        let text = "Let me think about this again. ".repeat(400);
        assert!(first_detection(&text).is_some());
    }

    #[test]
    fn a_loop_of_multibyte_text_is_stopped() {
        let text = "Vänta, låt mig räkna om det här steget. ".repeat(200);
        assert!(first_detection(&text).is_some());
    }

    #[test]
    fn whitespace_alone_is_not_a_loop() {
        assert_eq!(first_detection(&"\n".repeat(20_000)), None);
    }

    #[test]
    fn progress_forgets_what_came_before() {
        let paragraph = derivation(700);
        let mut watch = RepetitionWatch::default();
        for _ in 0..2 {
            assert!(watch.feed(&paragraph).is_none());
        }
        watch.reset();
        assert!(watch.feed(&paragraph).is_none());
        assert!(watch.feed(&paragraph).is_none());
    }
}
