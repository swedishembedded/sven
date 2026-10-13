// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The SDK's harness object: a YAML-defined agent harness, loaded, run,
//! checkpointed and resumed without writing Rust for it.
//!
//! A harness is a compiled [`HarnessPlan`]: bounded steps, declared inputs
//! and outputs, declared capabilities and limits. [`Engine::load_harness`]
//! parses and compiles it offline - the same bar every child spawn clears -
//! and registers it on the engine's [`HarnessRegistry`], so `delegate` and
//! `parallel` steps resolve it by name. [`Harness::start`] builds a
//! [`HarnessRun`]: one independent kernel session driven by a
//! [`HarnessMachine`] through [`RuntimeBuilder::with_machine`] - an ordinary
//! kernel machine wired by the ordinary builder, with the plan's declared
//! capabilities as its permission policy. Nothing about the run is new
//! machinery: the same executors, the same tool registry, the same child
//! spawner a mode-registered session gets, so many runs go through one
//! engine concurrently.
//!
//! A run reports a typed [`HarnessOutcome`]: the plan's own verdict (a
//! `return` value or a `fail` code, read from the run's outcome fact, never
//! inferred), how the run ended, the structured trace, and the token usage.
//! A run stopped by a bound leaves a [`HarnessCheckpoint`] - the kernel
//! snapshot, pinned to the digest it was taken under - and
//! [`Harness::resume`] continues a checkpoint whose plan (and, for a bundle
//! run, whose helpers) still match. The operation a suspension interrupted
//! did not complete; on resume it is settled through the machine's own
//! failure path, so the step's declared retry policy decides what repeating
//! it is worth.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use sven_harness_runtime::{HarnessMachine, HarnessRegistry};
use sven_harness_spec::{HarnessBundle, HarnessPlan};
use sven_hsm::Snapshot;

use crate::engine::Engine;
use crate::error::CallError;

mod run;

pub use run::{HarnessFailure, HarnessOutcome, HarnessRun};

impl Engine {
    /// Loads a harness from a YAML file and registers it on this engine.
    ///
    /// The file is either a bare harness document or a bundle (top-level
    /// `document` and `helpers` keys). It is parsed, validated and compiled
    /// offline, and registered under the plan's own name, so this engine's
    /// sessions resolve it.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the file cannot be read, is not
    /// valid YAML, or its parse, validation or compile fails; a name that
    /// is already registered is refused (a registry is a set, not a
    /// scratchpad).
    pub fn load_harness(&self, path: impl AsRef<Path>) -> Result<Harness, CallError> {
        let text = std::fs::read_to_string(path.as_ref()).map_err(|e| {
            CallError::Precondition(format!("cannot read {}: {e}", path.as_ref().display()))
        })?;
        self.load_harness_from_str(&text)
    }

    /// Loads a harness from YAML text - a bare document, or a bundle whose
    /// top level carries `document` (and optionally `helpers`).
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the text is not valid YAML, or its
    /// parse, validation or compile fails; a name that is already
    /// registered is refused.
    pub fn load_harness_from_str(&self, yaml: &str) -> Result<Harness, CallError> {
        let refuse = |detail: String| CallError::Precondition(detail);
        let (plan, source, bundle_digest) = match bundle_or_document(yaml).map_err(refuse)? {
            Either::Bundle(bundle) => {
                let compiled = bundle.compile().map_err(|e| refuse(e.to_string()))?;
                let plan = Arc::new(compiled.plan);
                let helpers = bundle.helpers.clone();
                self.harnesses
                    .register_compiled(Arc::clone(&plan), helpers)
                    .map_err(|e| refuse(e.to_string()))?;
                (plan, HarnessSource::Bundle(bundle), Some(compiled.digest))
            }
            Either::Document(text) => {
                let plan = Arc::new(
                    sven_harness_spec::compile_str(&text).map_err(|e| refuse(e.to_string()))?,
                );
                self.harnesses
                    .register_compiled(Arc::clone(&plan), Default::default())
                    .map_err(|e| refuse(e.to_string()))?;
                (plan, HarnessSource::Document(text), None)
            }
        };
        Ok(Harness {
            engine: self.clone(),
            plan,
            source,
            bundle_digest,
        })
    }

    /// The registry every session this engine builds resolves harness
    /// children against.
    #[must_use]
    pub fn harness_registry(&self) -> Arc<HarnessRegistry> {
        Arc::clone(&self.harnesses)
    }
}

/// What a [`Harness`] was loaded from: the source a resume re-compiles and
/// pins against.
#[derive(Clone)]
enum HarnessSource {
    /// A document plus its helpers. A changed helper is a changed program.
    Bundle(HarnessBundle),
    /// A bare document; it carries no helpers.
    Document(String),
}

/// Distinguishes a bundle from a bare harness document: a bundle's top
/// level carries a `document` key.
fn bundle_or_document(yaml: &str) -> Result<Either, String> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| format!("not valid YAML: {e}"))?;
    match &value {
        serde_yaml::Value::Mapping(mapping) if mapping.contains_key("document") => {
            let bundle = serde_yaml::from_value::<HarnessBundle>(value)
                .map_err(|e| format!("not a valid bundle: {e}"))?;
            Ok(Either::Bundle(bundle))
        }
        _ => Ok(Either::Document(yaml.to_string())),
    }
}

enum Either {
    Bundle(HarnessBundle),
    Document(String),
}

/// A harness this engine can run: the compiled plan, plus the source a
/// resume re-compiles and pins against.
#[derive(Clone)]
pub struct Harness {
    engine: Engine,
    plan: Arc<HarnessPlan>,
    source: HarnessSource,
    /// The bundle's content digest, when the harness carries helpers: the
    /// identity a fresh run pins itself to.
    bundle_digest: Option<String>,
}

/// A suspended harness run: the kernel snapshot, pinned to the plan (and,
/// for a bundle run, the bundle) it was taken under.
#[derive(Clone)]
pub struct HarnessCheckpoint {
    pub(crate) snapshot: Snapshot,
    /// The digest of the plan the run ran under.
    pub plan_digest: String,
    /// The digest of the bundle the run pinned, when it ran from one.
    pub bundle_digest: Option<String>,
    /// The id the run published its events under; a resume keeps it, so
    /// the two episodes' events read as one run's.
    pub run_id: String,
}

impl Harness {
    /// The plan's own name: what `delegate` and `parallel` steps resolve.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.plan.name
    }

    /// The plan's digest: the identity a checkpoint pins.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.plan.digest
    }

    /// The compiled plan: entry, inputs, outputs, capabilities, limits,
    /// steps.
    #[must_use]
    pub fn plan(&self) -> &HarnessPlan {
        &self.plan
    }

    /// Whether `inputs` satisfies the plan's declared inputs: every
    /// declared name present with a value of the declared type.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] naming every violated field.
    pub fn validate_inputs(&self, inputs: &Value) -> Result<(), CallError> {
        let Some(declared) = &self.plan.inputs else {
            return Ok(());
        };
        let Value::Object(object) = inputs else {
            return Err(CallError::Precondition(
                "harness inputs must be a JSON object".to_string(),
            ));
        };
        let mut problems = Vec::new();
        for (name, input) in declared {
            match object.get(name) {
                None => problems.push(format!("missing input `{name}`")),
                Some(value) if !input.ty.matches(value) => {
                    problems.push(format!("`{name}` must be {}", io_type_word(&input.ty)))
                }
                Some(_) => {}
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(CallError::Precondition(problems.join("; ")))
        }
    }

    /// Starts a run of this harness with `inputs`.
    ///
    /// The inputs are validated against the plan before anything starts -
    /// nothing is sent anywhere until they check out.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the inputs do not satisfy the
    /// plan.
    pub fn start(&self, inputs: Value) -> Result<HarnessRun, CallError> {
        self.validate_inputs(&inputs)?;
        let bundle = match &self.source {
            HarnessSource::Bundle(bundle) => Some(Arc::new(bundle.clone())),
            HarnessSource::Document(_) => None,
        };
        Ok(HarnessRun::starting(
            self.engine.clone(),
            Arc::clone(&self.plan),
            inputs,
            bundle,
            self.bundle_digest.clone(),
        ))
    }

    /// Resumes a checkpointed run of this harness.
    ///
    /// The checkpoint is rebuilt under this harness's own program: a plan -
    /// or, for a bundle run, a helper set - whose digest no longer matches
    /// the checkpoint's is refused offline, before anything runs.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the checkpoint belongs to another
    /// harness or to a changed program.
    pub fn resume(&self, checkpoint: HarnessCheckpoint) -> Result<HarnessRun, CallError> {
        if checkpoint.plan_digest != self.plan.digest {
            return Err(CallError::Precondition(format!(
                "the checkpoint was taken under plan digest {}, this harness is {}",
                checkpoint.plan_digest, self.plan.digest
            )));
        }
        let (machine, _) = match &self.source {
            HarnessSource::Document(text) => {
                HarnessMachine::restore(text.clone(), checkpoint.snapshot.clone())
            }
            HarnessSource::Bundle(bundle) => {
                HarnessMachine::restore_bundle(bundle, checkpoint.snapshot.clone())
            }
        }
        .map_err(|e| CallError::Precondition(format!("cannot resume: {e}")))?;
        let policy = HarnessMachine::permission_policy_for(machine.run());
        Ok(HarnessRun::resumed(
            self.engine.clone(),
            Arc::clone(&self.plan),
            Box::new(sven_hsm::Hsm::new(machine)),
            policy,
            checkpoint.snapshot,
            checkpoint.run_id,
        ))
    }
}

/// The plan's declared input types, in words a caller can act on.
fn io_type_word(ty: &sven_harness_spec::IOType) -> &'static str {
    match ty {
        sven_harness_spec::IOType::String => "a string",
        sven_harness_spec::IOType::Number => "a number",
        sven_harness_spec::IOType::Boolean => "a boolean",
        sven_harness_spec::IOType::Json => "any JSON value",
    }
}
