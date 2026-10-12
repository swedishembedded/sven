// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness registry: the named harness documents a session's children
//! may be.
//!
//! A `delegate` step and a `parallel` step name their child by harness
//! name; whoever assembles the session registers the documents those names
//! resolve against. Registration compiles and validates the document
//! offline — the same bar every run starts behind — so a name that
//! resolves always names a complete, bounded plan. A registry is a set,
//! not a scratchpad: a name registers once, and re-registering it is an
//! error rather than a silent swap under a running session.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::error::HarnessRunError;
use sven_harness_spec::HarnessPlan;

/// Named harness plans, resolved at child-spawn time.
#[derive(Default)]
pub struct HarnessRegistry {
    plans: RwLock<BTreeMap<String, Arc<HarnessPlan>>>,
}

impl HarnessRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses, validates and compiles `yaml`, registering the plan under
    /// its own name.
    ///
    /// # Errors
    ///
    /// The document's parse, validation or compile failure, or the name is
    /// already registered.
    pub fn register(&self, yaml: &str) -> Result<String, HarnessRunError> {
        self.register_plan(sven_harness_spec::compile_str(yaml)?)
    }

    /// Registers a compiled plan under its own name.
    ///
    /// # Errors
    ///
    /// The plan's name is already registered.
    pub fn register_plan(&self, plan: HarnessPlan) -> Result<String, HarnessRunError> {
        let name = plan.name.clone();
        let mut plans = self.plans.write().expect("registry lock poisoned");
        if plans.contains_key(&name) {
            return Err(HarnessRunError::DuplicateHarness(name));
        }
        plans.insert(name.clone(), Arc::new(plan));
        Ok(name)
    }

    /// The registered plan called `name`, if one is.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Arc<HarnessPlan>> {
        self.plans
            .read()
            .expect("registry lock poisoned")
            .get(name)
            .cloned()
    }

    /// Whether nothing is registered (a session with no harness children).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plans
            .read()
            .expect("registry lock poisoned")
            .is_empty()
    }

    /// The registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.plans
            .read()
            .expect("registry lock poisoned")
            .keys()
            .cloned()
            .collect()
    }
}
