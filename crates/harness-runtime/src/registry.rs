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
use sven_harness_spec::{HarnessBundle, HarnessPlan, Helper};

/// What a name resolves to: the compiled plan plus the helpers its
/// `transform` steps run. Harnesses registered from a bare document have
/// no helpers and fail such steps when reached; a registered bundle
/// carries its own.
struct Registered {
    plan: Arc<HarnessPlan>,
    helpers: BTreeMap<String, Helper>,
}

/// Named harness plans, resolved at child-spawn time.
#[derive(Default)]
pub struct HarnessRegistry {
    plans: RwLock<BTreeMap<String, Registered>>,
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

    /// Registers a bundle: the document is compiled against its helper set
    /// (so a `transform` step naming a helper the bundle does not carry is
    /// refused here, offline), and both the plan and the helpers register
    /// under the plan's own name.
    ///
    /// # Errors
    ///
    /// The document's parse, validation or compile failure, the bundle's
    /// used↔declared helper checks, or the name is already registered.
    pub fn register_bundle(&self, bundle: &HarnessBundle) -> Result<String, HarnessRunError> {
        let compiled = bundle.compile()?;
        let name = compiled.plan.name.clone();
        let mut plans = self.plans.write().expect("registry lock poisoned");
        if plans.contains_key(&name) {
            return Err(HarnessRunError::DuplicateHarness(name));
        }
        plans.insert(
            name.clone(),
            Registered {
                plan: Arc::new(compiled.plan),
                helpers: bundle.helpers.clone(),
            },
        );
        Ok(name)
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
        plans.insert(
            name.clone(),
            Registered {
                plan: Arc::new(plan),
                helpers: BTreeMap::new(),
            },
        );
        Ok(name)
    }

    /// Registers an already-compiled plan with its helper set.
    ///
    /// A host that compiled the plan offline — the same compile a child
    /// spawn clears — registers the result directly; the used↔declared
    /// helper checks already ran in that compile. Registration compiles
    /// nothing twice and refuses a name that is already taken, exactly as
    /// [`Self::register`](Self::register) does.
    ///
    /// # Errors
    ///
    /// The plan's name is already registered.
    pub fn register_compiled(
        &self,
        plan: Arc<HarnessPlan>,
        helpers: BTreeMap<String, Helper>,
    ) -> Result<String, HarnessRunError> {
        let name = plan.name.clone();
        let mut plans = self.plans.write().expect("registry lock poisoned");
        if plans.contains_key(&name) {
            return Err(HarnessRunError::DuplicateHarness(name));
        }
        plans.insert(name.clone(), Registered { plan, helpers });
        Ok(name)
    }

    /// The registered plan called `name`, if one is.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Arc<HarnessPlan>> {
        self.plans
            .read()
            .expect("registry lock poisoned")
            .get(name)
            .map(|registered| Arc::clone(&registered.plan))
    }

    /// The helpers a registered harness carries, by name. Empty for a
    /// harness registered from a bare document.
    #[must_use]
    pub fn helpers(&self, name: &str) -> BTreeMap<String, Helper> {
        self.plans
            .read()
            .expect("registry lock poisoned")
            .get(name)
            .map(|registered| registered.helpers.clone())
            .unwrap_or_default()
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
