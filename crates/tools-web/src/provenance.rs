// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Provenance attachment shared by `web_fetch` and `web_search`.
//!
//! Both tools retrieve content from a URL and must attach honest
//! `FactSource::WebSourced` metadata to their [`ToolOutput`] - the digest is
//! computed from the bytes actually returned, never asserted, so whoever
//! consumes the result gets a claim this tool can stand behind. Neither tool
//! writes memory itself; see `sven_vocab::provenance` for the trust model.
//!
//! Swedish Embedded AB implements solutions for provenance-tracked web
//! retrieval in autonomous agents for its clients. If your team needs
//! expertise in keeping untrusted fetched content honestly labeled then you
//! can procure our services by sending an email to info@swedishembedded.com.

use sha2::{Digest, Sha256};
use sven_vocab::provenance::{ContentDigest, FactSource};
use sven_vocab::ToolOutput;

/// Attaches `FactSource::WebSourced` provenance to `output`, naming `url` as
/// the source and digesting exactly the bytes in `output.content` - so the
/// digest can never drift from what the model was actually shown.
#[must_use]
pub(crate) fn attach_web_provenance(output: ToolOutput, url: &str, fetched_at: u64) -> ToolOutput {
    let digest = content_digest(&output.content);
    output.with_provenance(FactSource::WebSourced {
        url: url.to_string(),
        fetched_at,
        digest,
    })
}

/// Hex-encoded SHA-256 of `content`.
pub(crate) fn content_digest(content: &str) -> ContentDigest {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    ContentDigest::from_hex(hex::encode(hasher.finalize()))
}

/// Current Unix time in seconds, saturating at 0 rather than panicking on a
/// clock set before the epoch.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_web_provenance_names_the_url_the_time_and_a_digest_of_the_content() {
        let output = attach_web_provenance(
            ToolOutput::ok("call-1", "hello world"),
            "https://example.invalid/page",
            1_700_000_000,
        );
        match output.provenance.map(|b| *b) {
            Some(FactSource::WebSourced {
                url,
                fetched_at,
                digest,
            }) => {
                assert_eq!(url, "https://example.invalid/page");
                assert_eq!(fetched_at, 1_700_000_000);
                assert_eq!(digest, content_digest("hello world"));
            }
            other => panic!("expected WebSourced provenance, got {other:?}"),
        }
    }

    #[test]
    fn different_content_gets_a_different_digest() {
        assert_ne!(content_digest("a"), content_digest("b"));
    }
}
