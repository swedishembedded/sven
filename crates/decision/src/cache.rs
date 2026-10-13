// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A bounded, per-process cache of decisions.
//!
//! A decision is a pure function of the model's weights, the state and the
//! questions, so the same ask against the same weights has the same answer.
//! The key is therefore the model id, the identity of its checkpoint and the
//! normalised request. A model that does not report a checkpoint identity
//! cannot be versioned, so its decisions are never cached. The deadline is
//! not part of the key: it bounds the wait, not the answer.
//!
//! The cache lives in the process that owns it and holds at most the number
//! of entries it was built with, least recently used out first. Nothing is
//! persisted and nothing is shared between processes.

use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;

use crate::{DecisionRequest, DecisionResult, ModelIdentity};

/// What identifies one cached decision.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(String);

/// The cache.
pub struct DecisionCache {
    entries: Mutex<LruCache<CacheKey, DecisionResult>>,
}

impl DecisionCache {
    /// A cache holding at most `capacity` decisions.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: Mutex::new(LruCache::new(capacity)),
        }
    }

    /// The key of `request` as answered by `model`, or `None` when the model
    /// reports no checkpoint identity.
    #[must_use]
    pub fn key(model: &ModelIdentity, request: &DecisionRequest) -> Option<CacheKey> {
        let checkpoint = model.checkpoint.as_deref()?;
        let normalised = serde_json::json!({
            "state": request.state.replace("\r\n", "\n").trim_end(),
            "questions": request.questions,
        });
        Some(CacheKey(format!(
            "{}\u{0}{checkpoint}\u{0}{normalised}",
            model.id
        )))
    }

    /// The cached decision for `key`, marking it recently used.
    #[must_use]
    pub fn get(&self, key: &CacheKey) -> Option<DecisionResult> {
        self.entries.lock().unwrap().get(key).cloned()
    }

    /// Remembers `result`, dropping the least recently used decision when
    /// the cache is full.
    pub fn put(&self, key: CacheKey, result: DecisionResult) {
        self.entries.lock().unwrap().put(key, result);
    }

    /// How many decisions are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
