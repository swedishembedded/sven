// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Recording and offline replay of harness runs.
//!
//! A harness machine is deterministic: given the same events in the same
//! order, it walks the same steps and emits the same effects. A run's
//! complete input is therefore the log of events its machine was handed -
//! user kickoff, model answers, tool results, child verdicts, the host's
//! approvals. [`HarnessRun::with_recording`] captures that log by wrapping
//! the machine the kernel drives; [`Harness::replay`] re-dispatches it into
//! a fresh machine under the same program and executes none of the effects
//! the machine emits - no model is asked, no tool runs, no child starts.
//!
//! Swedish Embedded AB implements deterministic, auditable agent runtimes
//! for its clients. If your team needs expertise in reproducing and
//! debugging autonomous workflows then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_harness_runtime::{
    HarnessMachine, HarnessRunData, HarnessRunState, RunValues, TraceRecord, OUTCOME_FACT,
};
use sven_hsm::submachine::ErasedMachine;
use sven_hsm::{Context, DispatchOutcome, Effect, Event, Hsm, MachineId, RestoreError, Snapshot};

use super::run::HarnessFailure;
use super::run_support::outcome_of;
use super::{Harness, HarnessCheckpoint, HarnessRun, HarnessSource};
use crate::error::CallError;

/// The format tag of a [`HarnessRecording`]: a reader refuses a tag it does
/// not know rather than guess at the layout.
pub const RECORDING_FORMAT: &str = "sven-harness-recording/1";

/// The input log of one harness run: everything needed to reproduce its
/// verdict and trace offline, pinned to the program it ran under.
///
/// Serializes to JSON; [`Harness::replay`] consumes it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HarnessRecording {
    /// Always `RECORDING_FORMAT` for a recording this SDK writes.
    pub format: String,
    /// The name of the harness that ran.
    pub harness: String,
    /// The digest of the plan the run ran under.
    pub plan_digest: String,
    /// The digest of the bundle the run pinned, when it ran from one.
    pub bundle_digest: Option<String>,
    /// The id the run published its events under.
    pub run_id: String,
    /// The inputs the run started with.
    pub inputs: Value,
    /// What the host provided the run (its scratchpad, its trajectory file),
    /// which a fresh run's replay needs to read the same templates. A resumed
    /// run's checkpoint carries them.
    #[serde(default)]
    pub run_values: RunValues,
    /// The checkpoint a resumed run started from; `None` for a fresh run.
    pub start: Option<HarnessCheckpoint>,
    /// Every event the machine was dispatched, in order.
    pub events: Vec<Event>,
}

/// What an offline replay reached.
#[derive(Debug, Clone)]
pub struct HarnessReplay {
    /// The state the machine rests in after the last event.
    pub state: String,
    /// The plan's verdict, when the replay reached one.
    pub result: Option<Result<Value, HarnessFailure>>,
    /// The structured trace the replay produced.
    pub trace: Vec<TraceRecord>,
    /// How many step executions the replay made.
    pub steps: u64,
    /// Every effect the machine emitted, in order. Reported, never
    /// executed.
    pub effects: Vec<Effect>,
}

/// The recording of one run in progress: the shared event log, and the
/// checkpoint a resumed run began at.
#[derive(Clone)]
pub(super) struct Recorder {
    log: Arc<Mutex<Vec<Event>>>,
    start: Option<HarnessCheckpoint>,
}

impl Recorder {
    pub(super) fn new() -> Self {
        Self {
            log: Arc::default(),
            start: None,
        }
    }

    /// Wraps `machine` so every event it is dispatched lands in the log.
    /// `start` is the checkpoint a resumed run begins at.
    pub(super) fn attach(
        &mut self,
        machine: Box<dyn ErasedMachine>,
        start: Option<HarnessCheckpoint>,
    ) -> Box<dyn ErasedMachine> {
        self.start = start;
        Box::new(Recording {
            inner: machine,
            log: Arc::clone(&self.log),
        })
    }
}

/// The checkpoint a resumed run starts from, as its recording pins it.
pub(super) fn start_checkpoint(
    plan_digest: &str,
    run_id: &str,
    snapshot: &Snapshot,
) -> HarnessCheckpoint {
    HarnessCheckpoint {
        snapshot: snapshot.clone(),
        plan_digest: plan_digest.to_string(),
        bundle_digest: HarnessRunState::load(&snapshot.context).bundle_digest,
        run_id: run_id.to_string(),
    }
}

/// A machine that logs every event handed to it before delegating.
///
/// The log is the machine's complete input: effects are outputs, and the
/// kernel's lifecycle signals (`init`) are not events.
struct Recording {
    inner: Box<dyn ErasedMachine>,
    log: Arc<Mutex<Vec<Event>>>,
}

impl ErasedMachine for Recording {
    fn id(&self) -> MachineId {
        self.inner.id()
    }

    fn init(&mut self, ctx: &mut Context) -> Vec<Effect> {
        self.inner.init(ctx)
    }

    fn dispatch(&mut self, event: &Event, ctx: &mut Context) -> DispatchOutcome {
        // A poisoned log means another dispatch panicked; the run is
        // already lost, and recording must not add a second failure.
        if let Ok(mut log) = self.log.lock() {
            log.push(event.clone());
        }
        self.inner.dispatch(event, ctx)
    }

    fn state_label(&self) -> String {
        self.inner.state_label()
    }

    fn all_state_labels(&self) -> Vec<String> {
        self.inner.all_state_labels()
    }

    fn restore_state(&mut self, state: &str) -> Result<(), RestoreError> {
        self.inner.restore_state(state)
    }

    fn is_done(&self) -> bool {
        self.inner.is_done()
    }
}

impl HarnessRun {
    /// Turns recording on: the run keeps the log of events its machine was
    /// dispatched, retrievable with [`Self::recording`] after
    /// [`Self::execute`]. Call before executing; the log grows with the
    /// run, which its plan limits already bound.
    #[must_use]
    pub fn with_recording(mut self) -> Self {
        self.recorder.get_or_insert_with(Recorder::new);
        self
    }

    /// The recording of an executed run that [`Self::with_recording`]
    /// turned recording on for.
    #[must_use]
    pub fn recording(&self) -> Option<HarnessRecording> {
        let recorder = self.recorder.as_ref()?;
        if !self.executed {
            return None;
        }
        let events = recorder.log.lock().ok()?.clone();
        let (inputs, bundle_digest) = match &recorder.start {
            Some(start) => (
                HarnessRunState::load(&start.snapshot.context).inputs,
                start.bundle_digest.clone(),
            ),
            None => (self.inputs.clone(), self.bundle_digest.clone()),
        };
        Some(HarnessRecording {
            format: RECORDING_FORMAT.to_string(),
            harness: self.plan.name.clone(),
            plan_digest: self.plan.digest.clone(),
            bundle_digest,
            run_id: self.run_id.clone(),
            inputs,
            run_values: self.run_values.clone(),
            start: recorder.start.clone(),
            events,
        })
    }
}

impl Harness {
    /// Replays a recording offline.
    ///
    /// The recorded events are re-dispatched into a fresh machine under
    /// this harness's program (a resumed run's recording starts from its
    /// checkpoint). The effects the machine emits are collected, never
    /// executed: nothing asks a model, runs a tool or starts a child, so a
    /// replay needs no provider and no tools, and the same recording always
    /// yields the same [`HarnessReplay`].
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the recording is of an unknown
    /// format, was taken under another plan or bundle than this harness's,
    /// or its inputs or checkpoint do not fit the program.
    pub fn replay(&self, recording: &HarnessRecording) -> Result<HarnessReplay, CallError> {
        let refuse = |detail: String| CallError::Precondition(detail);
        if recording.format != RECORDING_FORMAT {
            return Err(refuse(format!(
                "unknown recording format `{}`, expected `{RECORDING_FORMAT}`",
                recording.format
            )));
        }
        if recording.plan_digest != self.plan.digest {
            return Err(refuse(format!(
                "the recording was taken under plan digest {}, this harness is {}",
                recording.plan_digest, self.plan.digest
            )));
        }
        if recording.bundle_digest != self.bundle_digest {
            return Err(refuse(format!(
                "the recording was taken under bundle {:?}, this harness is {:?}",
                recording.bundle_digest, self.bundle_digest
            )));
        }

        let (mut machine, mut ctx) = match &recording.start {
            Some(start) => self.restored_for_replay(start)?,
            None => {
                self.validate_inputs(&recording.inputs)?;
                let data = match &self.source {
                    HarnessSource::Bundle(bundle) => {
                        HarnessRunData::from_bundle(bundle, recording.inputs.clone())
                            .map_err(|e| refuse(format!("cannot replay: {e}")))?
                    }
                    HarnessSource::Document(_) => {
                        HarnessRunData::from_parts((*self.plan).clone(), recording.inputs.clone())
                    }
                };
                let data = data.with_run_values(recording.run_values.clone());
                (Hsm::new(HarnessMachine::new(data)), Context::new())
            }
        };

        let mut effects = machine.init(&mut ctx);
        for event in &recording.events {
            effects.extend(machine.dispatch(event, &mut ctx).effects);
        }

        let state = machine.state_label();
        let run_state = HarnessRunState::load(&ctx);
        Ok(HarnessReplay {
            result: outcome_of(&state, ctx.fact(OUTCOME_FACT)),
            state,
            trace: run_state.trace,
            steps: run_state.counts.steps,
            effects,
        })
    }

    /// The machine and context a resumed run's replay begins from: the
    /// checkpoint rebuilt under this program, exactly as a resume does.
    fn restored_for_replay(
        &self,
        start: &HarnessCheckpoint,
    ) -> Result<(Hsm<HarnessMachine>, Context), CallError> {
        let snapshot = start.snapshot.clone();
        let (machine, ctx) = match &self.source {
            HarnessSource::Document(text) => HarnessMachine::restore(text.clone(), snapshot),
            HarnessSource::Bundle(bundle) => HarnessMachine::restore_bundle(bundle, snapshot),
        }
        .map_err(|e| CallError::Precondition(format!("cannot replay: {e}")))?;
        let mut machine = Hsm::new(machine);
        machine
            .restore_in_place(&start.snapshot.state)
            .map_err(|e| CallError::Precondition(format!("cannot replay: {e}")))?;
        Ok((machine, ctx))
    }
}
