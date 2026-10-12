// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The SDK's harness object: a YAML-defined agent harness, loaded, run,
//! checkpointed and resumed without writing Rust for it.
//!
//! A harness is a compiled [`HarnessPlan`]: bounded steps, declared inputs
//! and outputs, declared capabilities and limits. [`Engine::load_harness`]
//! parses and compiles it offline — the same bar every child spawn clears —
//! and registers it on the engine's [`HarnessRegistry`], so `delegate` and
//! `parallel` steps resolve it by name. [`Harness::start`] builds a
//! [`HarnessRun`]: one independent kernel session driven by a
//! [`HarnessMachine`] through [`RuntimeBuilder::with_machine`] — an ordinary
//! kernel machine wired by the ordinary builder, with the plan's declared
//! capabilities as its permission policy. Nothing about the run is new
//! machinery: the same executors, the same tool registry, the same child
//! spawner a mode-registered session gets, so many runs go through one
//! engine concurrently.
//!
//! A run reports a typed [`HarnessOutcome`]: the plan's own verdict (a
//! `return` value or a `fail` code, read from the run's outcome fact, never
//! inferred), how the run ended, the structured trace, and the token usage.
//! A run stopped by a bound leaves a [`HarnessCheckpoint`] — the kernel
//! snapshot, pinned to the digest it was taken under — and
//! [`Harness::resume`] continues a checkpoint whose plan (and, for a bundle
//! run, whose helpers) still match. The operation a suspension interrupted
//! did not complete; on resume it is settled through the machine's own
//! failure path, so the step's declared retry policy decides what repeating
//! it is worth.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sven_bootstrap::{RuntimeBuilder, RuntimeContext};
use sven_harness_runtime::{
    HarnessMachine, HarnessRegistry, HarnessRunData, HarnessRunState, TraceRecord, Waiting,
    OUTCOME_FACT,
};
use sven_harness_spec::{HarnessBundle, HarnessPlan};
use sven_hsm::submachine::ErasedMachine;
use sven_hsm::{Context, EffectKind, Event, Snapshot};
use sven_vocab::SessionEvent;
use tokio::sync::broadcast;

use crate::agent::ask_through;
use crate::engine::Engine;
use crate::error::CallError;
use crate::run::{RunConclusion, RunOptions, Usage};

/// How long a stopped run is given to report what it had before it is
/// left — the same grace a mode session's wind-down gets.
const WIND_DOWN: Duration = Duration::from_secs(5);

/// How many captures may be served with nothing processed in between
/// before a resting run is judged stuck.
const STALLED_CAPTURES: u32 = 128;

/// The mode string a harness run builds under. The machine and its policy
/// come from [`RuntimeBuilder::with_machine`]; the mode only picks the
/// built-in tool profile's default, which the engine's toolset overrides.
const HARNESS_MODE: &str = "agent";

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

    /// Loads a harness from YAML text — a bare document, or a bundle whose
    /// top level carries `document` (and optionally `helpers`).
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the text is not valid YAML, or its
    /// parse, validation or compile fails; a name that is already
    /// registered is refused.
    pub fn load_harness_from_str(&self, yaml: &str) -> Result<Harness, CallError> {
        let refuse = |detail: String| CallError::Precondition(detail);
        let (plan, source) = match bundle_or_document(yaml).map_err(refuse)? {
            Either::Bundle(bundle) => {
                let compiled = bundle.compile().map_err(|e| refuse(e.to_string()))?;
                let plan = Arc::new(compiled.plan);
                let helpers = bundle.helpers.clone();
                self.harnesses
                    .register_compiled(Arc::clone(&plan), helpers)
                    .map_err(|e| refuse(e.to_string()))?;
                (plan, HarnessSource::Bundle(bundle))
            }
            Either::Document(text) => {
                let plan = Arc::new(
                    sven_harness_spec::compile_str(&text).map_err(|e| refuse(e.to_string()))?,
                );
                self.harnesses
                    .register_compiled(Arc::clone(&plan), Default::default())
                    .map_err(|e| refuse(e.to_string()))?;
                (plan, HarnessSource::Document(text))
            }
        };
        Ok(Harness {
            engine: self.clone(),
            plan,
            source,
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
    /// The inputs are validated against the plan before anything starts —
    /// nothing is sent anywhere until they check out.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when the inputs do not satisfy the
    /// plan.
    pub fn start(&self, inputs: Value) -> Result<HarnessRun, CallError> {
        self.validate_inputs(&inputs)?;
        Ok(HarnessRun::starting(
            self.engine.clone(),
            Arc::clone(&self.plan),
            inputs,
        ))
    }

    /// Resumes a checkpointed run of this harness.
    ///
    /// The checkpoint is rebuilt under this harness's own program: a plan —
    /// or, for a bundle run, a helper set — whose digest no longer matches
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
        ))
    }
}

/// One execution of a harness: an independent kernel session, run to a
/// terminal outcome or a bound.
///
/// A run executes once; the way to run again is a new [`Harness::start`],
/// and the way to continue a stopped run is [`Harness::resume`]. Runs are
/// independent: many can be in flight through one engine at once.
pub struct HarnessRun {
    engine: Engine,
    plan: Arc<HarnessPlan>,
    /// A fresh run's inputs (unset for a resumed one — the restored run
    /// state carries them).
    inputs: Value,
    /// The machine a resume starts from, with its policy and snapshot.
    restored: Option<RestoredMachine>,
    events: broadcast::Sender<SessionEvent>,
    trace_tx: broadcast::Sender<TraceRecord>,
    /// The checkpoint of a run stopped by a bound, after [`Self::execute`].
    checkpoint: Option<HarnessCheckpoint>,
    executed: bool,
}

struct RestoredMachine {
    machine: Box<dyn ErasedMachine>,
    policy: sven_hsm::PermissionPolicy,
    snapshot: Snapshot,
    /// How many trace records the snapshot already carried: the live
    /// stream continues after them.
    trace_len: usize,
}

impl HarnessRun {
    fn starting(engine: Engine, plan: Arc<HarnessPlan>, inputs: Value) -> Self {
        Self::new(engine, plan, inputs, None)
    }

    fn resumed(
        engine: Engine,
        plan: Arc<HarnessPlan>,
        machine: Box<dyn ErasedMachine>,
        policy: sven_hsm::PermissionPolicy,
        snapshot: Snapshot,
    ) -> Self {
        let trace_len = HarnessRunState::load(&snapshot.context).trace.len();
        Self::new(
            engine,
            plan,
            Value::Null,
            Some(RestoredMachine {
                machine,
                policy,
                snapshot,
                trace_len,
            }),
        )
    }

    fn new(
        engine: Engine,
        plan: Arc<HarnessPlan>,
        inputs: Value,
        restored: Option<RestoredMachine>,
    ) -> Self {
        Self {
            engine,
            plan,
            inputs,
            restored,
            events: broadcast::channel(256).0,
            trace_tx: broadcast::channel(256).0,
            checkpoint: None,
            executed: false,
        }
    }

    /// Subscribes to the session events the run's kernel publishes.
    pub fn events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// Subscribes to the run's structured lifecycle trace: one record per
    /// completed step, retry, child, join and failure, in order. A resumed
    /// run's stream continues after the records its checkpoint carried.
    pub fn subscribe_trace(&self) -> broadcast::Receiver<TraceRecord> {
        self.trace_tx.subscribe()
    }

    /// The checkpoint of a run that a bound stopped, after
    /// [`Self::execute`].
    ///
    /// [`Harness::resume`] continues it under the same program.
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
        let resumed = self.restored.take();

        // A project root is discovered from the way the CLI discovers its
        // project, and confines the built-in tools; without one the session
        // knows no project at all.
        let paths = self.engine.paths();
        let discovered = match paths.root() {
            Some(root) => RuntimeContext::auto_detect_at(Some(root.to_path_buf())),
            None => RuntimeContext::default(),
        };
        let runtime_ctx = RuntimeContext {
            path_scope: paths,
            ..discovered
        };

        // The turn executor parks the abort sender for the model call in
        // flight here; taking it interrupts that call.
        let abort_slot = Arc::new(tokio::sync::Mutex::new(None));
        let mut builder = RuntimeBuilder::new(self.engine.config(), HARNESS_MODE)
            .with_cancel_handle(Arc::clone(&abort_slot))
            .with_allow_interactive_oauth(false)
            .with_runtime_context(runtime_ctx)
            .with_builtin_tools(self.engine.toolset().builtin())
            .with_extra_tools(self.engine.tools())
            .with_approval_mode(self.engine.approvals().into())
            .with_harnesses(self.engine.harness_registry());
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
        let kickoff = match resumed.as_ref() {
            None => Event::user_message("start"),
            Some(restored) => {
                match interrupted_failure(&HarnessRunState::load(&restored.snapshot.context)) {
                    Some(event) => event,
                    None => Event::user_message("start"),
                }
            }
        };

        // The machine and the policy it answers for. A fresh run's policy is
        // the plan's declared capabilities; a resumed run's comes with the
        // machine.
        let (machine, policy) = match resumed {
            Some(restored) => {
                builder = builder.with_kernel_snapshot(restored.snapshot.clone());
                (restored.machine, restored.policy)
            }
            None => {
                let data = HarnessRunData::from_parts((*self.plan).clone(), self.inputs.clone());
                let policy = HarnessMachine::permission_policy_for(&data);
                (
                    Box::new(sven_hsm::Hsm::new(HarnessMachine::new(data)))
                        as Box<dyn ErasedMachine>,
                    policy,
                )
            }
        };
        builder = builder.with_machine(machine, policy);

        let bundle = builder
            .build_session()
            .await
            .map_err(CallError::Infrastructure)?;
        match human {
            Some(responder) => tokio::spawn(bundle.channels.forward_to(responder)),
            None => tokio::spawn(bundle.channels.answer_unattended()),
        };

        if !bundle.handle.sink().emit(kickoff).await {
            return Err(CallError::Infrastructure(anyhow::anyhow!(
                "kernel event queue closed before the run was started"
            )));
        }

        let mut usage = Usage::default();
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
        let deadline = tokio::time::sleep(bounds.deadline.unwrap_or(Duration::MAX / 4));
        tokio::pin!(deadline);
        // How long a stopped run may take to wind down before it is left.
        let mut wind_down = std::pin::pin!(tokio::time::sleep(Duration::MAX / 4));

        // The select loop borrows the runtime for its capture requests; it
        // must be over — the borrow dropped — before the run's final state
        // is joined below.
        {
            let mut observations = bundle.handle.subscribe_observations();
            // A capture is served when the kernel has nothing left to do; it
            // carries the trace as it stands, which is what the live stream
            // publishes from. `None` means the consumer task is gone — the
            // run is over, and the final state comes from the join below.
            let mut settled = Box::pin(bundle.runtime.capture());

            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled(), if stopped.is_none() => {
                        stopped = Some(RunConclusion::Cancelled);
                        settle_cancel(&bundle.handle, &abort_slot, wind_down.as_mut()).await;
                    }
                    () = &mut deadline, if stopped.is_none() && bounds.deadline.is_some() => {
                        stopped = Some(RunConclusion::Timeout);
                        bundle.runtime.cancel();
                        wind_down.as_mut().reset(tokio::time::Instant::now() + WIND_DOWN);
                    }
                    () = &mut wind_down, if stopped.is_some() => break,
                    event = observations.recv() => match event {
                        Ok(event) => {
                            match &event {
                                SessionEvent::TokenUsage { input, output, .. } => {
                                    usage.add(*input, *output);
                                    let spent = usage.output_tokens.unwrap_or(0);
                                    if stopped.is_none()
                                        && bounds.max_output_tokens.is_some_and(|max| spent >= max)
                                    {
                                        stopped = Some(RunConclusion::BudgetExhausted);
                                        bundle.runtime.cancel();
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
                    captured = settled.as_mut() => match captured {
                        Some(snapshot) => {
                            publish_trace(&snapshot.context, &mut emitted_trace, &self.trace_tx);
                            // A capture is only served when the machine has
                            // nothing left to do; two in a row over the very
                            // same processed count mean it can no longer make
                            // progress — most plausibly it ignored the event
                            // that was meant to settle it. Report that rather
                            // than re-arming forever.
                            let processed = bundle.runtime.status().processed;
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
                                    bundle.runtime.cancel();
                                    wind_down.as_mut().reset(
                                        tokio::time::Instant::now() + WIND_DOWN,
                                    );
                                }
                            } else {
                                idle_captures = 0;
                            }
                            last_processed = processed;
                            settled = Box::pin(bundle.runtime.capture());
                        }
                        None => break,
                    },
                }
            }
        }

        // The report carries the machine's final state and context — the
        // outcome fact, the trace, the counts — whether the run ended on
        // its own or was stopped by a bound.
        let report = bundle
            .runtime
            .join()
            .await
            .map_err(|e| CallError::Infrastructure(anyhow::anyhow!(e)))?;
        let run_state = HarnessRunState::load(&report.ctx);
        publish_trace(&report.ctx, &mut emitted_trace, &self.trace_tx);
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
            self.checkpoint = Some(HarnessCheckpoint {
                snapshot: Snapshot {
                    state: report.state_label.clone(),
                    context: report.ctx,
                },
                plan_digest: run_state
                    .plan_digest
                    .clone()
                    .unwrap_or_else(|| self.plan.digest.clone()),
                bundle_digest: run_state.bundle_digest.clone(),
            });
        }
        Ok(HarnessOutcome {
            conclusion: stopped.unwrap_or(RunConclusion::Success),
            result,
            usage,
            trace: run_state.trace,
            steps: run_state.counts.steps,
        })
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
    /// message; the cancelled failure for a cancelled run. `None` — with
    /// [`HarnessRun::checkpoint`] — when a bound suspended the run before
    /// it reached one.
    pub result: Option<Result<Value, HarnessFailure>>,
    /// Token usage across the run, children included.
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

/// Cancels a run: the machine is told the user cancelled, settles into its
/// own terminal cancelled state — propagating the cancellation through the
/// children it started — and reports a cancelled verdict. Nothing is left
/// to resume.
async fn settle_cancel(
    handle: &sven_bootstrap::RuntimeHandle,
    abort_slot: &tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    wind_down: std::pin::Pin<&mut tokio::time::Sleep>,
) {
    if let Some(abort) = abort_slot.lock().await.take() {
        let _ = abort.send(());
    }
    let _ = handle.cancel().await;
    wind_down.reset(tokio::time::Instant::now() + WIND_DOWN);
}

/// Publishes the trace records `ctx` gained since the last publish.
fn publish_trace(ctx: &Context, emitted: &mut usize, trace_tx: &broadcast::Sender<TraceRecord>) {
    let state = HarnessRunState::load(ctx);
    for record in state.trace.iter().skip(*emitted) {
        let _ = trace_tx.send(record.clone());
    }
    *emitted = state.trace.len();
}

/// The verdict a finished run reported, read off its terminal state and
/// its outcome fact. `None` when the run has no verdict: a bound stopped
/// it first.
fn outcome_of(state_label: &str, outcome: Option<&Value>) -> Option<Result<Value, HarnessFailure>> {
    match state_label {
        "Completed" => Some(Ok(outcome.cloned().unwrap_or(Value::Null))),
        "Failed" => {
            let code = outcome
                .and_then(|o| o.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("failed")
                .to_string();
            let message = outcome
                .and_then(|o| o.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the run failed")
                .to_string();
            Some(Err(HarnessFailure { code, message }))
        }
        "Cancelled" => Some(Err(HarnessFailure {
            code: "cancelled".to_string(),
            message: "the user cancelled the run".to_string(),
        })),
        _ => None,
    }
}

/// The event that settles what a suspension interrupted, from what the
/// run was waiting on.
///
/// A resumed run must not re-execute the parked step (the tick rule
/// forbids a second child for the same `delegate`); it must answer the
/// operation that was in flight when the suspension hit, and answer it as
/// a failure, so the step's declared retry policy decides what becomes of
/// it. Which event answers depends on the waiter: the tool arm only
/// accepts the result of the exact call it parked on, so it needs the
/// call id the run recorded; the model, verifier, child and join arms
/// settle on a failed effect.
fn interrupted_failure(state: &HarnessRunState) -> Option<Event> {
    const SUSPENDED: &str =
        "the run was suspended with this operation in flight; it did not complete";
    let inflight = state.inflight.as_ref()?;
    match state.awaiting? {
        Waiting::Model => Some(Event::LlmFailed {
            error: SUSPENDED.to_string(),
        }),
        Waiting::Tool => Some(Event::ToolFailed {
            call_id: inflight.call_id?,
            error: SUSPENDED.to_string(),
        }),
        Waiting::Verifier => Some(Event::EffectFailed {
            kind: EffectKind::Verify,
            error: SUSPENDED.to_string(),
        }),
        Waiting::Child | Waiting::Join => Some(Event::EffectFailed {
            kind: EffectKind::InstantiateSubmachine,
            error: SUSPENDED.to_string(),
        }),
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
