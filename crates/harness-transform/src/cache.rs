// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A bounded cache of compiled helper modules, keyed by the SHA-256 of
//! their bytes: decode and validate once, instantiate per invocation.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::error::TransformError;
use crate::wasm::Module;

/// Compiled helper modules by content hash. A miss decodes and validates
/// the bytes (the expensive static gate); a hit reuses the module, while
/// each invocation still gets its own fresh instance. The cache is bounded
/// by entry count and evicts the oldest entry when full — recompiling is
/// merely slow, never incorrect.
pub struct ModuleCache {
    max_entries: usize,
    entries: HashMap<[u8; 32], Arc<Module>>,
    order: VecDeque<[u8; 32]>,
}

impl ModuleCache {
    /// A cache holding at most `max_entries` modules.
    pub fn bounded(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// The compiled module for these bytes, decoding and validating on a
    /// miss.
    pub fn load(&mut self, wasm: &[u8]) -> Result<Arc<Module>, TransformError> {
        self.load_hashed(crate::digest(wasm), wasm)
    }

    /// The compiled module for bytes whose digest the caller already took
    /// — one hash when a caller needs the digest itself.
    pub fn load_hashed(
        &mut self,
        key: [u8; 32],
        wasm: &[u8],
    ) -> Result<Arc<Module>, TransformError> {
        if let Some(module) = self.entries.get(&key) {
            return Ok(Arc::clone(module));
        }
        let module = Arc::new(crate::compile_module(wasm)?);
        if self.entries.len() >= self.max_entries {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, Arc::clone(&module));
        self.order.push_back(key);
        Ok(module)
    }

    /// How many modules are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
