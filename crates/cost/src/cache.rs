// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! What the provider's prompt cache is expected to still hold.

use sven_vocab::ReportedUsage;

/// Tokens of the prompt prefix expected to be served from the cache.
///
/// A cache is warm for a time-to-live after its last use and then gone, so the
/// expectation is all or nothing and errs on the cold side: an idle time that
/// is not known is treated as expired, never as fresh.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheState {
    pub warm_tokens: u32,
}

fn fresh(ttl_secs: Option<u32>, idle_secs: Option<u64>) -> bool {
    matches!((ttl_secs, idle_secs), (Some(ttl), Some(idle)) if idle <= u64::from(ttl))
}

impl CacheState {
    /// The whole `prefix_tokens` is warm when the last request was at most
    /// `ttl_secs` ago; none of it otherwise.
    #[must_use]
    pub fn expected(prefix_tokens: u32, ttl_secs: Option<u32>, idle_secs: Option<u64>) -> Self {
        Self {
            warm_tokens: if fresh(ttl_secs, idle_secs) {
                prefix_tokens
            } else {
                0
            },
        }
    }

    /// What the last request left in the cache (what it read and wrote), if
    /// that was at most `ttl_secs` ago. This is the provider's own report, so
    /// it is preferred over an estimate whenever a request has been made.
    #[must_use]
    pub fn after(last: &ReportedUsage, ttl_secs: Option<u32>, idle_secs: Option<u64>) -> Self {
        Self {
            warm_tokens: if fresh(ttl_secs, idle_secs) {
                last.cache_read.saturating_add(last.cache_write)
            } else {
                0
            },
        }
    }
}
