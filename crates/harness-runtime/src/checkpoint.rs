// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Suspending and resuming a harness run.
//!
//! A checkpoint is the tracked leaf plus the context — the whole run state
//! travels inside it. Resuming rebuilds the machine from the same program
//! the checkpoint was taken under: a bare run pins its plan's digest, a
//! bundle run pins the bundle's (a changed helper is a changed program),
//! and a document or bundle that no longer matches is refused offline
//! rather than replaying recorded results through changed steps.

use sven_harness_spec::HarnessBundle;
use sven_hsm::{Context, MachineId, Snapshot, ToolCapability};

use crate::error::HarnessRunError;
use crate::machine::{capability_by_name, HarnessMachine};
use crate::run::{HarnessRunData, HarnessRunState};
use crate::state::HarnessState;

impl HarnessMachine {
    /// Suspends the run into a serializable [`Snapshot`]: the tracked leaf
    /// plus the context (which carries the whole run state under
    /// [`RUN_STATE_FACT`](crate::RUN_STATE_FACT)).
    #[must_use]
    pub fn snapshot(&self, ctx: &Context) -> Snapshot {
        Snapshot {
            state: format!("{:?}", self.leaf),
            context: ctx.clone(),
        }
    }

    /// Rebuilds a machine from `yaml` and resumes it from `snap`: the walk
    /// continues from the snapshotted state and run data; no entry action
    /// re-fires and no completed step re-executes.
    ///
    /// The snapshot's run state records the digest of the plan the run
    /// started under; a document that compiles to a different digest is not
    /// the checkpointed plan and is refused — recorded results must never be
    /// replayed through changed steps.
    ///
    /// # Errors
    ///
    /// [`HarnessRunError::Spec`] when `yaml` no longer parses or compiles;
    /// [`HarnessRunError::PinnedPlanMismatch`] when the document is a
    /// modified version of the checkpointed plan;
    /// [`HarnessRunError::Unrestorable`] when the snapshot names a state
    /// this machine cannot rest in.
    pub fn restore(yaml: String, snap: Snapshot) -> Result<(Self, Context), HarnessRunError> {
        let resumed = HarnessRunState::load(&snap.context);
        let plan = sven_harness_spec::compile_str(&yaml)?;
        if let Some(checkpoint) = &resumed.plan_digest {
            if plan.digest != *checkpoint {
                return Err(HarnessRunError::PinnedPlanMismatch {
                    name: plan.name.clone(),
                    checkpoint: checkpoint.clone(),
                    document: plan.digest.clone(),
                });
            }
        }
        if resumed.bundle_digest.is_some() {
            return Err(HarnessRunError::PinnedBundleMismatch {
                name: plan.name.clone(),
                checkpoint: resumed.bundle_digest.unwrap_or_default(),
                offered: "a bare document".to_string(),
            });
        }
        Self::resume(
            HarnessRunData::from_parts(plan, resumed.inputs.clone()),
            &snap,
        )
    }

    /// Rebuilds a machine from a bundle and resumes it from `snap`: the
    /// same contract as [`HarnessMachine::restore`], with the bundle the
    /// checkpoint may pin. The document is compiled against the helper set
    /// (offline), and a checkpoint taken under a different bundle digest is
    /// refused — a changed helper is a changed program, exactly as a
    /// changed document is.
    ///
    /// # Errors
    ///
    /// Everything [`HarnessMachine::restore`] can produce, plus
    /// [`HarnessRunError::PinnedBundleMismatch`] when the checkpoint was
    /// taken under a different bundle.
    pub fn restore_bundle(
        bundle: &HarnessBundle,
        snap: Snapshot,
    ) -> Result<(Self, Context), HarnessRunError> {
        let compiled = bundle.compile()?;
        let resumed = HarnessRunState::load(&snap.context);
        if let Some(checkpoint) = &resumed.plan_digest {
            if compiled.plan.digest != *checkpoint {
                return Err(HarnessRunError::PinnedPlanMismatch {
                    name: compiled.plan.name.clone(),
                    checkpoint: checkpoint.clone(),
                    document: compiled.plan.digest.clone(),
                });
            }
        }
        if let Some(checkpoint) = &resumed.bundle_digest {
            if compiled.digest != *checkpoint {
                return Err(HarnessRunError::PinnedBundleMismatch {
                    name: compiled.plan.name.clone(),
                    checkpoint: checkpoint.clone(),
                    offered: format!("digest {digest}", digest = compiled.digest),
                });
            }
        }
        let data = HarnessRunData::from_parts(compiled.plan, resumed.inputs.clone())
            .with_helpers(bundle.helpers.clone())
            .pinned_to_bundle(compiled.digest);
        Self::resume(data, &snap)
    }

    /// Shared tail of the restore paths: resolve the snapshot's state
    /// label and rebuild the machine.
    fn resume(run: HarnessRunData, snap: &Snapshot) -> Result<(Self, Context), HarnessRunError> {
        let leaf = crate::state::resolve_label(&snap.state)
            .ok_or_else(|| HarnessRunError::Unrestorable(snap.state.clone()))?;
        let machine = Self {
            id: MachineId::new(),
            run,
            leaf,
            step_effect_cache: None,
            outcome_cache: None,
            #[cfg(feature = "transform")]
            helper_modules: sven_harness_transform::cache::ModuleCache::bounded(64),
        };
        Ok((machine, snap.context.clone()))
    }

    /// The labels a snapshot of this machine may name: every state but the
    /// root, which a running machine never rests in.
    #[must_use]
    pub fn restorable_state_labels(&self) -> Vec<String> {
        crate::state::ALL_STATES
            .into_iter()
            .filter(|s| *s != HarnessState::Root)
            .map(|s| format!("{s:?}"))
            .collect()
    }

    /// The permission policy of a harness run: exactly the plan's declared
    /// capabilities, allowed in every state. An undeclared capability is
    /// granted nowhere — the plan is the whole contract, and nothing in the
    /// runtime widens it.
    #[must_use]
    pub fn permission_policy_for(run: &HarnessRunData) -> sven_hsm::PermissionPolicy {
        let declared: Vec<ToolCapability> = run
            .plan()
            .capabilities
            .iter()
            .filter_map(|name| capability_by_name(name))
            .collect();
        sven_hsm::PermissionPolicy::builder()
            .allow_globally(declared)
            .build()
    }
}
