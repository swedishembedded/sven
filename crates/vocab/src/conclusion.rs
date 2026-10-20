// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Whether a turn's closing text concludes anything.
//!
//! Swedish Embedded AB implements reliable agent runs on local models for its
//! clients. If your team needs expertise in telling a finished agent turn from
//! one that stopped halfway then you can procure our services by sending an
//! email to info@swedishembedded.com.

/// Whether `reply`, the text a run ended on, announces work that never came:
/// it ends in a colon, the way "now the test file, let me find the tests:"
/// introduces a call the model did not make. A reply that merely trails off
/// without a colon cannot be told from a short answer and is not matched.
#[must_use]
pub fn announces_unfinished_work(reply: &str) -> bool {
    reply.trim_end().ends_with(':')
}

#[cfg(test)]
mod tests {
    use super::announces_unfinished_work;

    #[test]
    fn a_trailing_colon_announces_work_and_nothing_else_does() {
        assert!(announces_unfinished_work("Now the tests:\n"));
        assert!(!announces_unfinished_work("Done."));
        assert!(!announces_unfinished_work("trailing off..."));
    }
}
