// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One harness execution: the kernel session a [`HarnessRun`] drives, its
//! outcome, and the trace it publishes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sven_bootstrap::{RuntimeBuilder, RuntimeContext};
use sven_harness_runtime::{
    apply_to_snapshot, HarnessMachine, HarnessRunData, HarnessRunState, RunValues, TraceRecord,
    OUTCOME_FACT,
};
use sven_harness_spec::{HarnessBundle, HarnessPlan, PlanIteration, PlanPrimitive};
use sven_hsm::submachine::ErasedMachine;
use sven_hsm::{Event, Snapshot};
use sven_vocab::{HarnessEvent, SessionEvent};
use tokio::sync::broadcast;

use super::live_trajectory;
use super::record::{start_checkpoint, Recorder};
use super::run_support::{
    checkpoint_at, harness_tools, interrupted_failure, outcome_of, publish_trace, settle_cancel,
    WIND_DOWN,
};
use super::scratchpad::{Choices, Scratchpad};
use super::trajectory::Evidence;
use super::HarnessCheckpoint;
use crate::engine::Engine;
use crate::error::CallError;
use crate::gates::{ask_through, forward};
use crate::meter::Meter;
use crate::run::{RunConclusion, RunOptions, Usage};

/// How many captures may be served with nothing processed in between
/// before a resting run is judged stuck.
const STALLED_CAPTURES: u32 = 128;

/// The mode string a harness run builds under. The machine and its policy
/// come from [`RuntimeBuilder::with_machine`]; the mode only picks the
/// built-in tool profile's default, which the engine's toolset overrides.
const HARNESS_MODE: &str = "agent";

/// One execution of a harness: an independent kernel session, run to a
/// terminal outcome or a bound.
///
/// A run executes once; the way to run again is a new [`super::Harness::start`],
/// and the way to continue a stopped run is [`super::Harness::resume`]. Runs are
/// independent: many can be in flight through one engine at once.
pub struct HarnessRun {
    engine: Engine,
    pub(super) plan: Arc<HarnessPlan>,
    /// A fresh run's inputs (unset for a resumed one - the restored run
    /// state carries them).
    pub(super) inputs: Value,
    /// The bundle a fresh run builds from, when the harness carries
    /// helpers: the helpers travel with the run and the run pins the
    /// bundle's digest.
    bundle: Option<Arc<HarnessBundle>>,
    /// The pinned bundle digest (unset for a document-only harness and a
    /// resumed run, whose snapshot state carries its own).
    pub(super) bundle_digest: Option<String>,
    /// The id this run's events are published under.
    pub(super) run_id: String,
    /// The machine a resume starts from, with its policy and snapshot.
    restored: Option<RestoredMachine>,
    events: broadcast::Sender<SessionEvent>,
    trace_tx: broadcast::Sender<TraceRecord>,
    /// The checkpoint of a run stopped by a bound, after [`Self::execute`].
    checkpoint: Option<HarnessCheckpoint>,
    pub(super) executed: bool,
    /// The input log, when recording is on (see `record.rs`).
    pub(super) recorder: Option<Recorder>,
    /// What the trajectory is built from, after [`Self::execute`].
    pub(super) evidence: Option<Evidence>,
    /// The directory this run works in, when it is not the engine's.
    workspace: Option<sven_tool_api::PathScope>,
    /// Told of a checkpoint whenever a `for_each` item has completed.
    item_checkpoint: Option<ItemCheckpointSink>,
    /// Whether the trajectory file a gate reads outlives the run.
    keep_trajectory_file: bool,
    /// The directory the host chose for this run, if it chose one.
    scratchpad_choice: Option<std::path::PathBuf>,
    /// What the host provided the run, known once it executes.
    pub(super) run_values: RunValues,
}

/// Receives the checkpoint a host asked for at each completed work item.
type ItemCheckpointSink = Box<dyn Fn(HarnessCheckpoint) + Send + Sync>;

struct RestoredMachine {
    machine: Box<dyn ErasedMachine>,
    policy: sven_hsm::PermissionPolicy,
    snapshot: Snapshot,
    /// How many trace records the snapshot already carried: the live
    /// stream continues after them.
    trace_len: usize,
}

impl HarnessRun {
    pub(super) fn starting(
        engine: Engine,
        plan: Arc<HarnessPlan>,
        inputs: Value,
        bundle: Option<Arc<HarnessBundle>>,
        bundle_digest: Option<String>,
    ) -> Self {
        Self::new(engine, plan, inputs, bundle, bundle_digest, None, None)
    }

    pub(super) fn resumed(
        engine: Engine,
        plan: Arc<HarnessPlan>,
        machine: Box<dyn ErasedMachine>,
        policy: sven_hsm::PermissionPolicy,
        snapshot: Snapshot,
        run_id: String,
    ) -> Self {
        let trace_len = HarnessRunState::load(&snapshot.context).trace.len();
        Self::new(
            engine,
            plan,
            Value::Null,
            None,
            None,
            Some(RestoredMachine {
                machine,
                policy,
                snapshot,
                trace_len,
            }),
            Some(run_id),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        engine: Engine,
        plan: Arc<HarnessPlan>,
        inputs: Value,
        bundle: Option<Arc<HarnessBundle>>,
        bundle_digest: Option<String>,
        restored: Option<RestoredMachine>,
        run_id: Option<String>,
    ) -> Self {
        Self {
            engine,
            plan,
            inputs,
            bundle,
            bundle_digest,
            run_id: run_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            restored,
            events: broadcast::channel(256).0,
            trace_tx: broadcast::channel(256).0,
            checkpoint: None,
            executed: false,
            recorder: None,
            evidence: None,
            workspace: None,
            item_checkpoint: None,
            keep_trajectory_file: false,
            scratchpad_choice: None,
            run_values: RunValues::default(),
        }
    }

    /// The data a fresh run starts from: the bundle's, with its helpers and
    /// digest pinned, or the plan's alone.
    fn fresh_data(&self) -> Result<HarnessRunData, CallError> {
        match &self.bundle {
            Some(bundle) => HarnessRunData::from_bundle(bundle, self.inputs.clone())
                .map_err(|e| CallError::Precondition(format!("cannot start: {e}"))),
            None => Ok(HarnessRunData::from_parts(
                (*self.plan).clone(),
                self.inputs.clone(),
            )),
        }
    }

    /// Subscribes to the session events the run's kernel publishes.
    pub fn events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// The id this run's events are published under. A resumed run keeps
    /// the id its checkpoint carried, so its two episodes' events read as
    /// one run's.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Subscribes to the run's structured lifecycle trace: one record per
    /// completed step, retry, child, join and failure, in order. A resumed
    /// run's stream continues after the records its checkpoint carried.
    pub fn subscribe_trace(&self) -> broadcast::Receiver<TraceRecord> {
        self.trace_tx.subscribe()
    }

    /// Runs this harness in `dir` instead of the engine's project root: the
    /// run's file, search and shell tools, its gates and its snapshot reads are
    /// all rooted there, so one engine can serve many runs at once, each in its
    /// own tree (the host lays the tree out before the run and reads it after;
    /// the harness never creates it). Tools the host registered on the engine
    /// itself keep the root they were built with.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when `dir` is not an existing directory.
    pub fn in_workspace(mut self, dir: impl AsRef<std::path::Path>) -> Result<Self, CallError> {
        let scope = sven_tool_api::PathScope::confined(dir.as_ref()).map_err(|e| {
            CallError::Precondition(format!(
                "workspace {} is not usable: {e}",
                dir.as_ref().display()
            ))
        })?;
        self.workspace = Some(scope);
        Ok(self)
    }

    /// Gives the run the private directory `dir` (`{{ run.scratchpad }}`),
    /// created if it is missing. Without this a run uses the directory its
    /// checkpoint recorded, else the one its document names
    /// (`execution.scratchpad`), else a fresh one under the system temporary
    /// directory. It must be an absolute path that is neither inside the
    /// workspace nor holds it; [`Self::execute`] refuses it otherwise.
    #[must_use]
    pub fn with_scratchpad(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.scratchpad_choice = Some(dir.into());
        self
    }

    /// The run's private directory, once [`Self::execute`] has settled it.
    /// It holds the run's audit log (`audit.jsonl`) and whatever its gates
    /// leave, and is kept after the run.
    #[must_use]
    pub fn scratchpad(&self) -> Option<&std::path::Path> {
        self.run_values
            .scratchpad
            .as_deref()
            .map(std::path::Path::new)
    }

    /// Keeps the trajectory file (`{{ run.trajectory_file }}`), final after the run;
    /// by default it is removed when the run ends, however it ends.
    #[must_use]
    pub fn keep_trajectory_file(mut self) -> Self {
        self.keep_trajectory_file = true;
        self
    }

    /// Hands `sink` a checkpoint each time a `for_each` item completes, so a
    /// host can keep the latest one durable: a run killed without warning
    /// then resumes at the item it was on ([`super::Harness::resume`]) and
    /// never repeats a completed one. The checkpoint is taken the next time
    /// the machine is at rest after the item, so it can already include
    /// work on the following one.
    #[must_use]
    pub fn on_item_checkpoint(
        mut self,
        sink: impl Fn(HarnessCheckpoint) + Send + Sync + 'static,
    ) -> Self {
        self.item_checkpoint = Some(Box::new(sink));
        self
    }

    /// Gives the host's item-checkpoint sink the run as it rests at
    /// `snapshot`, when an item has just completed.
    fn checkpoint_item(&self, completed_loops: &[String], snapshot: &Snapshot) {
        let marked = completed_loops.iter().any(|step| {
            matches!(
                self.plan.step(step).map(|s| &s.primitive),
                Some(PlanPrimitive::Iterate {
                    iteration: PlanIteration::ForEach {
                        checkpoint_each_item: true,
                        ..
                    },
                    ..
                })
            )
        });
        if let (true, Some(sink)) = (marked, &self.item_checkpoint) {
            sink(checkpoint_at(
                snapshot.clone(),
                &self.plan.digest,
                &self.run_id,
            ));
        }
    }

    /// The checkpoint of a run that a bound stopped, after
    /// [`Self::execute`].
    ///
    /// [`super::Harness::resume`] continues it under the same program.
    #[must_use]
    pub fn checkpoint(&self) -> Option<HarnessCheckpoint> {
        self.checkpoint.clone()
    }

    /// Runs the harness to a terminal outcome or a bound.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the run already executed;
    /// [`CallError::Infrastructure`] when the kernel session could not be
    /// built or died without the machine reporting an outcome.
    pub async fn execute(&mut self, bounds: RunOptions) -> Result<HarnessOutcome, CallError> {
        if self.executed {
            return Err(CallError::Precondition(
                "this run already executed; start a new run or resume its checkpoint".to_string(),
            ));
        }
        self.executed = true;
        let mut resumed = self.restored.take();

        // A project root is discovered from the way the CLI discovers its
        // project, and confines the built-in tools; without one the session
        // knows no project at all.
        let paths = self
            .workspace
            .clone()
            .unwrap_or_else(|| self.engine.paths());
        let root = paths.root().map(std::path::Path::to_path_buf);
        let discovered = match paths.root() {
            Some(root) => RuntimeContext::auto_detect_at(Some(root.to_path_buf())),
            None => RuntimeContext::default(),
        };
        let runtime_ctx = RuntimeContext {
            path_scope: paths,
            // An embedded engine never opens the user's own memory store; a
            // host that wants memory adds its tool with `Engine::builder().tool`.
            memory_home: None,
            ..discovered
        };

        // The run's private directory, settled before anything is built on it.
        // A fresh run's data is built here because the document's scratchpad
        // is a template over its inputs.
        let mut restored_state = resumed
            .as_ref()
            .map(|restored| HarnessRunState::load(&restored.snapshot.context));
        let fresh = match resumed {
            Some(_) => None,
            None => Some(self.fresh_data()?),
        };
        let recorded = restored_state
            .as_ref()
            .and_then(|state| state.run_values().scratchpad);
        let document = match fresh.as_ref().map(HarnessRunData::document_scratchpad) {
            Some(Err(why)) => return Err(CallError::Precondition(why)),
            Some(Ok(dir)) => dir,
            None => None,
        };
        let scratch = Scratchpad::resolve(&Choices {
            host: self.scratchpad_choice.as_deref(),
            recorded: recorded.as_deref(),
            document,
            run_id: &self.run_id,
            workspace: root.as_deref(),
        })?;
        let reads_trajectory = sven_harness_runtime::plan_reads_trajectory(&self.plan);
        self.run_values = scratch.run_values(reads_trajectory);
        if let Some(restored) = resumed.as_mut() {
            // A resumed run continues in the directory it now has, which is
            // the recorded one unless the host chose another.
            apply_to_snapshot(&mut restored.snapshot, &self.run_values);
            restored_state = Some(HarnessRunState::load(&restored.snapshot.context));
        }

        // The turn executor parks the abort sender for the model call in
        // flight here; taking it interrupts that call.
        let abort_slot = Arc::new(tokio::sync::Mutex::new(None));
        // The children this run starts leave their records here, for the
        // trajectory to embed.
        let children = sven_bootstrap::child_records::ChildRecords::default();
        let trajectory_slot = live_trajectory::Slot::default();
        let mut builder = RuntimeBuilder::new(self.engine.config(), HARNESS_MODE)
            .with_cancel_handle(Arc::clone(&abort_slot))
            .with_decoding(bounds.decoding)
            .with_child_records(children.clone())
            .with_allow_interactive_oauth(false)
            .with_runtime_context(runtime_ctx)
            .with_audit_log_path(scratch.audit_log())
            .with_builtin_tools(self.engine.toolset().builtin())
            .with_extra_tools(harness_tools(
                root.as_deref(),
                self.engine.tools(),
                live_trajectory::hook(&trajectory_slot),
            ))
            .with_approval_mode(self.engine.approvals().into())
            .with_harnesses(self.engine.harness_registry());
        if let Some(roles) = self.engine.model_roles() {
            builder = builder.with_model_roles(roles);
        }
        if self.engine.parks_questions() {
            builder = builder.with_parked_questions();
        }
        let human = self.engine.human();
        // The model's `ask_question` goes to the host's handler, unless the
        // host chose to park it.
        if let (Some(responder), false) = (&human, self.engine.parks_questions()) {
            builder = builder.with_tool_question_tx(ask_through(Arc::clone(responder)));
        }
        if let Some(registry) = self.engine.machines() {
            builder = builder.with_mode_registry(registry);
        }
        if let Some(provider) = self.engine.provider() {
            builder = builder.with_shared_model_provider(provider);
        }

        // What starts (or continues) the machine. A fresh run is kicked with
        // the child spawner's own kickoff message; a resumed run either
        // continues a parked walk (a message tick) or settles the operation
        // its suspension interrupted (a failed-effect event, which the
        // step's declared retry policy answers).
        let resumed_trace_len = resumed.as_ref().map_or(0, |r| r.trace_len);
        let kickoff = match &restored_state {
            Some(state) => match interrupted_failure(state) {
                Some(event) => event,
                None => Event::user_message("start"),
            },
            None => Event::user_message("start"),
        };
        // What the run is running and what it started with, as its first
        // event names them. A resumed run reads both off its snapshot
        // state; a fresh run knows them from its harness.
        let (run_inputs, run_bundle_digest) = match &restored_state {
            Some(state) => (state.inputs.clone(), state.bundle_digest.clone()),
            None => (self.inputs.clone(), self.bundle_digest.clone()),
        };

        // The machine and the policy it answers for. A fresh run's policy is
        // the plan's declared capabilities; a resumed run's comes with the
        // machine.
        let start = self
            .recorder
            .as_ref()
            .zip(resumed.as_ref())
            .map(|(_, restored)| {
                start_checkpoint(&self.plan.digest, &self.run_id, &restored.snapshot)
            });
        let (mut machine, policy) = match resumed {
            Some(restored) => {
                builder = builder.with_kernel_snapshot(restored.snapshot.clone());
                (restored.machine, restored.policy)
            }
            None => {
                // A bundle run travels with its helpers and pins the
                // bundle's digest, exactly as a resume does: a fresh start
                // is not a way to run a bundle without its program.
                let data = fresh
                    .ok_or_else(|| {
                        CallError::Infrastructure(anyhow::anyhow!(
                            "a fresh run's data is built before its session"
                        ))
                    })?
                    .with_run_values(self.run_values.clone());
                let policy = HarnessMachine::permission_policy_for(&data);
                (
                    Box::new(sven_hsm::Hsm::new(HarnessMachine::new(data)))
                        as Box<dyn ErasedMachine>,
                    policy,
                )
            }
        };
        if let Some(recorder) = self.recorder.as_mut() {
            machine = recorder.attach(machine, start);
        }
        // The host's ceiling narrows the document's declaration, a fresh
        // run's and a resumed run's alike; it never adds to it.
        let policy = match self.engine.host_policy() {
            Some(ceiling) => policy.intersect(&ceiling),
            None => policy,
        };
        builder = builder.with_machine(machine, policy);

        let session = builder
            .build_session()
            .await
            .map_err(CallError::Infrastructure)?;
        match human {
            Some(handler) => tokio::spawn(forward(
                session.channels,
                crate::RunId::from_text(&self.run_id),
                handler,
            )),
            None => tokio::spawn(session.channels.answer_unattended()),
        };

        let live = reads_trajectory.then(|| {
            Arc::new(live_trajectory::Source {
                engine: self.engine.clone(),
                plan: Arc::clone(&self.plan),
                run_id: self.run_id.clone(),
                bundle_digest: self.bundle_digest.clone(),
                handle: session.handle.clone(),
                children: children.clone(),
                tap: session.runtime.snapshots(),
            })
        });
        let _dir = live.as_ref().map(|_| {
            live_trajectory::Cleanup::of(scratch.trajectory_file(), self.keep_trajectory_file)
        });
        if let Some(live) = &live {
            let writer = Arc::clone(live);
            let _ = trajectory_slot.set(Arc::new(move |path| writer.write(path)));
        }

        // The run announces itself before its first step: which program,
        // which helpers, which inputs.
        let _ = self.events.send(SessionEvent::Harness {
            run_id: self.run_id.clone(),
            event: HarnessEvent::Started {
                plan_digest: self.plan.digest.clone(),
                bundle_digest: run_bundle_digest,
                input_digest: HarnessPlan::canonical_digest(&run_inputs),
            },
        });

        if !session.handle.sink().emit(kickoff).await {
            return Err(CallError::Infrastructure(anyhow::anyhow!(
                "kernel event queue closed before the run was started"
            )));
        }

        let started = std::time::Instant::now();
        let mut meter = Meter::default();
        // The first hard failure the session reports. The machine's own
        // retry and failure handling answers effect failures; what reaches
        // here is what the machine could not absorb.
        let mut failure: Option<String> = None;
        // Set by the first bound that fires; the run then winds down and
        // reports it rather than the machine's own ending.
        let mut stopped: Option<RunConclusion> = None;
        let mut emitted_trace = resumed_trace_len;
        // Captures served with nothing having been processed in between: a
        // machine resting where nothing can reach it. Bounded so a stuck run
        // reports rather than spins.
        let mut idle_captures: u32 = 0;
        let mut last_processed: u64 = 0;
        let cancel = bounds.cancel.clone().unwrap_or_default();
        let suspend = bounds.suspend.clone().unwrap_or_default();
        let deadline = tokio::time::sleep(bounds.deadline.unwrap_or(Duration::MAX / 4));
        tokio::pin!(deadline);
        // How long a stopped run may take to wind down before it is left.
        let mut wind_down = std::pin::pin!(tokio::time::sleep(Duration::MAX / 4));
        // When the previous trace record was observed: a step's duration
        // is the gap between observations.
        let mut observed: Option<tokio::time::Instant> = None;

        // The select loop borrows the runtime for its capture requests; it
        // must be over - the borrow dropped - before the run's final state
        // is joined below.
        {
            let mut observations = session.handle.subscribe_observations();
            // A capture is served when the kernel has nothing left to do; it
            // carries the trace as it stands, which is what the live stream
            // publishes from. `None` means the consumer task is gone - the
            // run is over, and the final state comes from the join below.
            let mut settled = Box::pin(session.runtime.capture());
            // The tap shows the run after every dispatch, in the middle of a
            // long model turn too, where a capture would still be waiting:
            // the trace goes out, and item checkpoints are taken, as the run
            // goes. It is dropped from the select once the runtime is gone.
            let mut tap = session.runtime.snapshots();
            let mut tap_open = true;

            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled(), if stopped.is_none() => {
                        stopped = Some(RunConclusion::Cancelled);
                        settle_cancel(&session.handle, &abort_slot, wind_down.as_mut()).await;
                    }
                    () = suspend.cancelled(), if stopped.is_none() && bounds.suspend.is_some() => {
                        stopped = Some(RunConclusion::Timeout);
                        session.runtime.cancel();
                        wind_down.as_mut().reset(tokio::time::Instant::now() + WIND_DOWN);
                    }
                    () = &mut deadline, if stopped.is_none() && bounds.deadline.is_some() => {
                        stopped = Some(RunConclusion::Timeout);
                        session.runtime.cancel();
                        wind_down.as_mut().reset(tokio::time::Instant::now() + WIND_DOWN);
                    }
                    () = &mut wind_down, if stopped.is_some() => break,
                    event = observations.recv() => match event {
                        Ok(event) => {
                            meter.observe(&event, None);
                            match &event {
                                SessionEvent::TokenUsage { .. } => {
                                    let spent = meter.output_tokens();
                                    if stopped.is_none()
                                        && bounds.max_output_tokens.is_some_and(|max| spent >= max)
                                    {
                                        let _ = self.events.send(SessionEvent::Harness {
                                            run_id: self.run_id.clone(),
                                            event: HarnessEvent::BudgetExhausted {
                                                budget: "output_tokens".to_string(),
                                                consumed: spent,
                                                limit: bounds.max_output_tokens.unwrap_or(0),
                                            },
                                        });
                                        stopped = Some(RunConclusion::BudgetExhausted);
                                        session.runtime.cancel();
                                        wind_down.as_mut().reset(tokio::time::Instant::now() + WIND_DOWN);
                                    }
                                }
                                SessionEvent::Error(message) => {
                                    failure.get_or_insert_with(|| message.clone());
                                }
                                _ => {}
                            }
                            let _ = self.events.send(event);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    },
                    changed = tap.changed(), if tap_open => {
                        tap_open = changed.is_ok();
                        let latest = tap.borrow_and_update().clone();
                        if let Some(snapshot) = latest {
                            let completed_loops = publish_trace(
                                &snapshot.context,
                                &mut emitted_trace,
                                &self.trace_tx,
                                &self.events,
                                &self.run_id,
                                &mut observed,
                            );
                            self.checkpoint_item(&completed_loops, &snapshot);
                        }
                    },
                    captured = settled.as_mut() => match captured {
                        Some(snapshot) => {
                            let completed_loops = publish_trace(
                                &snapshot.context,
                                &mut emitted_trace,
                                &self.trace_tx,
                                &self.events,
                                &self.run_id,
                                &mut observed,
                            );
                            self.checkpoint_item(&completed_loops, &snapshot);
                            // A capture is only served when the machine has
                            // nothing left to do; two in a row over the very
                            // same processed count mean it can no longer make
                            // progress - most plausibly it ignored the event
                            // that was meant to settle it. Report that rather
                            // than re-arming forever.
                            let processed = session.runtime.status().processed;
                            if processed == last_processed {
                                idle_captures += 1;
                                if idle_captures > STALLED_CAPTURES {
                                    failure.get_or_insert_with(|| {
                                        format!(
                                            "the harness stopped making progress while \
                                             waiting in state {}",
                                            snapshot.state
                                        )
                                    });
                                    stopped.get_or_insert(RunConclusion::AgentError);
                                    session.runtime.cancel();
                                    wind_down.as_mut().reset(
                                        tokio::time::Instant::now() + WIND_DOWN,
                                    );
                                }
                            } else {
                                idle_captures = 0;
                            }
                            last_processed = processed;
                            settled = Box::pin(session.runtime.capture());
                        }
                        None => break,
                    },
                }
            }
        }

        // The report carries the machine's final state and context - the
        // outcome fact, the trace, the counts - whether the run ended on
        // its own or was stopped by a bound.
        let report = session
            .runtime
            .join()
            .await
            .map_err(|e| CallError::Infrastructure(anyhow::anyhow!(e)))?;
        let run_state = HarnessRunState::load(&report.ctx);
        publish_trace(
            &report.ctx,
            &mut emitted_trace,
            &self.trace_tx,
            &self.events,
            &self.run_id,
            &mut observed,
        );
        let result = outcome_of(&report.state_label, report.ctx.fact(OUTCOME_FACT));
        if stopped.is_none() && result.is_none() {
            return match failure {
                Some(message) => Err(CallError::Infrastructure(anyhow::anyhow!(message))),
                None => Err(CallError::Infrastructure(anyhow::anyhow!(
                    "the harness ended without reporting an outcome"
                ))),
            };
        }
        if result.is_none() {
            // The run was stopped before a verdict: checkpoint where it
            // rests. The program it ran under is this run's plan; the run
            // state may not have recorded the digest yet if it was stopped
            // before its first step.
            self.checkpoint = Some(checkpoint_at(
                Snapshot {
                    state: report.state_label.clone(),
                    context: report.ctx,
                },
                &self.plan.digest,
                &self.run_id,
            ));
        }
        let (usage, _) = meter.finish(started.elapsed());
        let outcome = HarnessOutcome {
            conclusion: stopped.unwrap_or(RunConclusion::Success),
            result,
            usage,
            trace: run_state.trace,
            steps: run_state.counts.steps,
        };
        let bundle = run_state.bundle_digest.as_deref();
        self.evidence = Some(Evidence::capture(
            &self.engine,
            &self.plan,
            &session.handle,
            &outcome,
            (bundle, &run_state.steps),
            children.finished(),
        ));
        if live.is_some() && self.keep_trajectory_file {
            live_trajectory::write_final(&scratch.trajectory_file(), self.trajectory());
        }
        Ok(outcome)
    }
}

/// The verdict of one harness run.
#[derive(Debug, Clone)]
pub struct HarnessOutcome {
    /// How the run ended. A run that reached its own `return` or `fail`
    /// ended on [`RunConclusion::Success`]: the run completed, whatever it
    /// decided. [`RunConclusion::Cancelled`] means the user cancelled it;
    /// the verdict is its cancelled failure. [`RunConclusion::Timeout`]
    /// and [`RunConclusion::BudgetExhausted`] mean an operational bound
    /// suspended it before a verdict.
    pub conclusion: RunConclusion,
    /// The plan's verdict: the `return` value, or the `fail` code and
    /// message; the cancelled failure for a cancelled run. `None` - with
    /// [`HarnessRun::checkpoint`] - when a bound suspended the run before
    /// it reached one.
    pub result: Option<Result<Value, HarnessFailure>>,
    /// What the run used, children included.
    pub usage: Usage,
    /// The structured execution trace: what the run did, in order.
    pub trace: Vec<TraceRecord>,
    /// How many step executions the run made.
    pub steps: u64,
}

/// A harness run's own failure: the code a `fail` step (or a bound the
/// plan itself exceeded) reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessFailure {
    /// The failure code the plan declared.
    pub code: String,
    /// What the failure means, for a person reading the outcome.
    pub message: String,
}
