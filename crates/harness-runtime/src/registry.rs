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
use sven_harness_spec::{HarnessBundle, HarnessPlan};

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

    /// Registers a bundle: its main document compiled with its imports, under
    /// the plan's own name.
    ///
    /// # Errors
    ///
    /// The document's parse, validation or compile failure, or the name is
    /// already registered.
    pub fn register_bundle(&self, bundle: &HarnessBundle) -> Result<String, HarnessRunError> {
        self.register_plan(bundle.compile()?.plan)
    }

    /// Registers a compiled plan under its own name.
    ///
    /// # Errors
    ///
    /// The plan's name is already registered.
    pub fn register_plan(&self, plan: HarnessPlan) -> Result<String, HarnessRunError> {
        self.register_compiled(Arc::new(plan))
    }

    /// Registers an already-compiled plan.
    ///
    /// A host that compiled the plan offline — the same compile a child
    /// spawn clears — registers the result directly. Registration compiles
    /// nothing twice and refuses a name that is already taken, exactly as
    /// [`Self::register`](Self::register) does.
    ///
    /// # Errors
    ///
    /// The plan's name is already registered.
    pub fn register_compiled(&self, plan: Arc<HarnessPlan>) -> Result<String, HarnessRunError> {
        let name = plan.name.clone();
        let mut plans = self.plans.write().expect("registry lock poisoned");
        if plans.contains_key(&name) {
            return Err(HarnessRunError::DuplicateHarness(name));
        }
        plans.insert(name.clone(), plan);
        Ok(name)
    }

    /// Registers a package: its main plan and the plans it imports, all or
    /// none. A name already taken, or taken twice within the set, refuses the
    /// whole set, so a refused load leaves nothing behind.
    ///
    /// # Errors
    ///
    /// A name is already registered, or repeats within `set`.
    pub fn register_package(&self, set: Vec<Arc<HarnessPlan>>) -> Result<String, HarnessRunError> {
        let mut plans = self.plans.write().expect("registry lock poisoned");
        let mut names = std::collections::BTreeSet::new();
        for plan in &set {
            if plans.contains_key(&plan.name) || !names.insert(plan.name.clone()) {
                return Err(HarnessRunError::DuplicateHarness(plan.name.clone()));
            }
        }
        // The main plan is the last of the set.
        let main = set.last().map(|plan| plan.name.clone()).unwrap_or_default();
        for plan in set {
            plans.insert(plan.name.clone(), plan);
        }
        Ok(main)
    }

    /// The registered plan called `name`, if one is.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Arc<HarnessPlan>> {
        self.plans
            .read()
            .expect("registry lock poisoned")
            .get(name)
            .map(Arc::clone)
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
