// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One agent: a conversation, its kernel state, and the engine it borrows.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sven_bootstrap::{RuntimeBuilder, RuntimeContext};
use sven_hsm::Event;
use sven_model::Message;
use sven_session_model::reduce_history;
use sven_vocab::SessionEvent;
use tokio::sync::broadcast;

use std::sync::Arc;
use std::time::Duration;

use sven_machines::machines::loop_core::MAX_ROUNDS_REACHED_FACT;

use crate::engine::{ApprovalPolicy, Engine};
use crate::error::CallError;
use crate::method::{Method, Strategy};
use crate::run::{RunConclusion, RunOptions, RunOutcome, Usage};
use crate::state::AgentState;

/// Capacity of the per-agent event broadcast channel.
const EVENT_CAPACITY: usize = 1024;

/// A live agent: a conversation in progress against an [`Engine`].
///
/// Cheap to create and cheap to drop. Everything expensive belongs to the
/// engine; everything durable belongs to the [`AgentState`] this can be
/// suspended into. A service typically creates one per request, advances it by
/// a step, and puts it back.
pub struct Agent {
    engine: Engine,
    state: AgentState,
    events: broadcast::Sender<SessionEvent>,
}

impl Agent {
    pub(crate) fn new(engine: Engine, state: AgentState) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            engine,
            state,
            events,
        }
    }

    /// Subscribes to everything this agent emits while it works.
    ///
    /// The same [`SessionEvent`] stream the TUI and the headless runner
    /// consume, so a custom surface renders progress without reaching into any
    /// kernel crate. Subscribe before calling [`Agent::send`]; a receiver
    /// created afterwards sees only what is still buffered.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// The agent's current state, including its history.
    #[must_use]
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Suspends the agent, yielding the state needed to resume it later.
    #[must_use]
    pub fn suspend(self) -> AgentState {
        self.state
    }

    /// Sends `text` to the agent and runs one turn with no bounds beyond the
    /// engine's own. See [`Self::send_with`].
    ///
    /// # Errors
    ///
    /// As [`Self::send_with`].
    pub async fn send(&mut self, text: &str) -> Result<RunOutcome, CallError> {
        self.send_with(text, RunOptions::default()).await
    }

    /// Sends `text` to the agent and runs one turn within `bounds`, returning
    /// how it ended, the reply and the tokens it used.
    ///
    /// The turn's history and ending kernel state are folded back into this
    /// agent, so a subsequent `send` - or a `send` after a suspend/resume
    /// round trip - continues the same conversation, also after a run that
    /// was cancelled or stopped by a bound.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if the agent's mode is not
    /// registered, and [`CallError::Infrastructure`] if the kernel session
    /// cannot be built, the event queue closes mid-turn, or the turn itself
    /// failed (the provider errored). A model that simply answers badly is
    /// not an error - it is the reply - and a run stopped by one of `bounds`
    /// is an outcome, not an error.
    pub async fn send_with(
        &mut self,
        text: &str,
        bounds: RunOptions,
    ) -> Result<RunOutcome, CallError> {
        self.run_turn(text, &TurnOptions::default(), &bounds).await
    }

    /// Calls a model-driven method with typed input and a validated result.
    ///
    /// The model call, the schema it is constrained by, and any correction
    /// attempts all happen behind this boundary. The caller sees a value of
    /// `T` or an explicit failure.
    ///
    /// The call is folded into this agent's history, so an agent used for
    /// several calls accumulates context across them. For a one-off call that
    /// should carry no history, use [`Engine::call`](crate::Engine::call).
    ///
    /// # Errors
    ///
    /// - [`CallError::Invalid`] if no answer could be read as `T` within the
    ///   method's repair budget.
    /// - [`CallError::Postcondition`] if answers were well-formed but kept
    ///   breaking the method's invariant.
    /// - [`CallError::Precondition`] if the input cannot be serialized.
    /// - [`CallError::Infrastructure`] if the kernel or provider failed -
    ///   never reported as a model mistake.
    pub async fn call<I, T>(&mut self, method: &Method<T>, input: &I) -> Result<T, CallError>
    where
        I: Serialize + ?Sized,
        T: DeserializeOwned + JsonSchema,
    {
        let rendered = serde_json::to_string_pretty(input).map_err(|e| {
            CallError::Precondition(format!(
                "the input to {:?} is not serializable: {e}",
                method.name()
            ))
        })?;

        let options = TurnOptions {
            role: self.state.role.clone().or_else(|| method.role.clone()),
            no_tools: method.strategy == Strategy::Predict,
            facts: Self::prediction_facts(method),
        };

        let mut message = method.instruction(&rendered);
        let mut attempts = 0;

        // Bounded correction. Each rejected answer stays in the thread and the
        // diagnostic is appended after it, so the model sees what it got wrong
        // rather than being asked again from a clean slate.
        loop {
            let last = self
                .run_turn(&message, &options, &RunOptions::default())
                .await?
                .reply;
            attempts += 1;

            // `structural` separates "could not be read as the type at all"
            // from "read fine, but broke an invariant" - two different reports
            // to the caller, and only the second proves the model understood
            // the shape it was asked for.
            let (structural, detail) = match parse_candidate::<T>(&last) {
                Ok(value) => match method.postcondition.as_ref().map(|c| c(&value)) {
                    None | Some(Ok(())) => return Ok(value),
                    Some(Err(why)) => (false, why),
                },
                Err(why) => (true, why),
            };

            if attempts > method.max_repairs {
                let type_name = std::any::type_name::<T>();
                return Err(if structural {
                    CallError::Invalid {
                        type_name,
                        attempts,
                        detail,
                        last,
                    }
                } else {
                    CallError::Postcondition {
                        type_name,
                        attempts,
                        detail,
                        last,
                    }
                });
            }

            message = format!(
                "That answer was rejected: {detail}\n\nReply again with nothing but a JSON \
                 object matching the required schema."
            );
        }
    }

    /// The context facts a `predict` session needs to constrain its turn.
    fn prediction_facts<T>(method: &Method<T>) -> serde_json::Map<String, serde_json::Value>
    where
        T: DeserializeOwned + JsonSchema,
    {
        let mut facts = serde_json::Map::new();
        if method.strategy == Strategy::Predict {
            facts.insert(
                sven_machines::machines::predict::SCHEMA_FACT.to_string(),
                method.schema(),
            );
            facts.insert(
                sven_machines::machines::predict::SCHEMA_NAME_FACT.to_string(),
                serde_json::Value::String(method.name().to_string()),
            );
        }
        facts
    }

    /// Builds a session, posts one message, and folds the result back in.
    async fn run_turn(
        &mut self,
        text: &str,
        options: &TurnOptions,
        bounds: &RunOptions,
    ) -> Result<RunOutcome, CallError> {
        if !self.engine.modes().contains(&self.state.mode) {
            return Err(CallError::Precondition(format!(
                "unknown mode {:?}; this engine can run {:?}",
                self.state.mode,
                self.engine.modes()
            )));
        }

        let runtime_ctx = RuntimeContext {
            system_prompt_override: options.role.clone(),
            no_tools: options.no_tools,
            ..RuntimeContext::default()
        };

        // The configured tool-round budget reaches the machine as a context
        // fact: `AgentConfig.max_tool_rounds` is the one place it is set, and
        // the reactive machine reads the fact on its first turn (falling back
        // to its own default when absent). Method facts keep their own keys
        // and win - they are seeded after this map is extended.
        let mut facts = options.facts.clone();
        facts.insert(
            sven_machines::MAX_TOOL_ROUNDS_FACT.to_string(),
            serde_json::json!(self.engine.config().agent.max_tool_rounds),
        );

        // The turn executor parks the abort sender for the model call in
        // flight here; taking it interrupts that call.
        let abort_slot = Arc::new(tokio::sync::Mutex::new(None));
        let mut builder = RuntimeBuilder::new(self.engine.config(), self.state.mode.clone())
            .with_cancel_handle(Arc::clone(&abort_slot))
            .with_allow_interactive_oauth(false)
            .with_runtime_context(runtime_ctx)
            .with_context_facts(facts)
            .with_builtin_tools(self.engine.toolset().builtin())
            .with_extra_tools(self.engine.tools())
            .with_initial_history(self.state.history.clone());
        if let Some(registry) = self.engine.machines() {
            builder = builder.with_mode_registry(registry);
        }
        if let Some(provider) = self.engine.provider() {
            builder = builder.with_shared_model_provider(provider);
        }
        if let Some(snapshot) = self.state.kernel.clone() {
            builder = builder.with_kernel_snapshot(snapshot);
        }

        let bundle = builder.build_session().await?;

        match self.engine.approvals() {
            ApprovalPolicy::AutoApprove => {
                tokio::spawn(bundle.channels.auto_approve());
            }
            ApprovalPolicy::Deny => {
                tokio::spawn(bundle.channels.deny_all());
            }
        }

        let mut observations = bundle.handle.subscribe_observations();
        self.state.history.push(Message::user(text));

        if !bundle
            .handle
            .sink()
            .emit(Event::UserMessage {
                text: text.to_string(),
            })
            .await
        {
            return Err(CallError::Infrastructure(anyhow::anyhow!(
                "kernel event queue closed before the message was delivered"
            )));
        }

        // A step ends when the kernel has nothing left to do. For a machine
        // that drives the model that is the end of its turn; for one that does
        // not - a custom machine answering from its own state, say - no turn
        // ever completes, and waiting for one would hang forever. Racing the
        // two means neither kind has to know about the other.
        //
        // `capture` is served only once the event queue has drained, so it is
        // exactly the "nothing left to do" signal. The select prefers
        // observations so that a model turn still ends on `TurnComplete`, with
        // its text collected, rather than on quiescence a moment later.
        let mut reply = String::new();
        let settled = bundle.runtime.capture();
        tokio::pin!(settled);
        let mut snapshot = None;
        // The FIRST hard failure of the turn, if any. `SessionEvent::Error` is
        // reserved for exactly that - a turn that merely produced nothing is
        // an ordinary empty turn and never reports one - so seeing it means
        // the turn did not do what it was asked. Keeping the first rather than
        // the last keeps the cause instead of whatever it cascaded into.
        //
        // This has to be tracked separately from `reply` because the executor
        // emits `Error` and then `TurnComplete`: without it, a turn that
        // failed is indistinguishable here from a turn that answered with an
        // empty string, and `send` returns `Ok("")` for both.
        let mut failure: Option<String> = None;
        let mut usage = Usage::default();
        // Set by the first bound that fires; the run then winds down and
        // reports it rather than `Success`.
        let mut stopped: Option<RunConclusion> = None;
        let cancel = bounds.cancel.clone().unwrap_or_default();
        let deadline = tokio::time::sleep(bounds.deadline.unwrap_or(Duration::MAX / 4));
        tokio::pin!(deadline);
        // How long a stopped run may take to wind down before it is left.
        let mut wind_down = std::pin::pin!(tokio::time::sleep(Duration::MAX / 4));

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled(), if stopped.is_none() => {
                    stopped = Some(RunConclusion::Cancelled);
                    stop(&bundle.handle, &abort_slot, wind_down.as_mut()).await;
                }
                () = &mut deadline, if stopped.is_none() && bounds.deadline.is_some() => {
                    stopped = Some(RunConclusion::Timeout);
                    stop(&bundle.handle, &abort_slot, wind_down.as_mut()).await;
                }
                () = &mut wind_down, if stopped.is_some() => break,
                event = observations.recv() => match event {
                    Ok(event) => {
                        let done = matches!(
                            event,
                            SessionEvent::TurnComplete | SessionEvent::Aborted { .. }
                        );
                        match &event {
                            SessionEvent::TokenUsage { input, output, .. } => {
                                usage.add(*input, *output);
                                let spent = usage.output_tokens.unwrap_or(0);
                                if stopped.is_none()
                                    && bounds.max_output_tokens.is_some_and(|max| spent >= max)
                                {
                                    stopped = Some(RunConclusion::BudgetExhausted);
                                    stop(&bundle.handle, &abort_slot, wind_down.as_mut()).await;
                                }
                            }
                            SessionEvent::TextComplete(text) => reply.push_str(text),
                            SessionEvent::Aborted { partial_text } => reply.push_str(partial_text),
                            SessionEvent::Error(message) => {
                                failure.get_or_insert_with(|| message.clone());
                            }
                            _ => {}
                        }
                        reduce_history(&event, &mut self.state.history);
                        let _ = self.events.send(event);
                        if done {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                },
                captured = &mut settled => {
                    snapshot = captured;
                    break;
                }
            }
        }

        // Capture where the kernel ended up before the session is torn down, so
        // the next step resumes here instead of re-entering from the top. When
        // the loop ended on a completed turn the in-flight request was dropped,
        // so ask again.
        self.state.kernel = match snapshot {
            Some(snapshot) => Some(snapshot),
            None => bundle.runtime.capture().await,
        };

        // Report the failure only after the kernel state above is stored, so a
        // caller that retries or inspects the agent resumes from where the
        // turn actually stopped rather than from before it started. The text
        // collected before the failure is folded into the error rather than
        // returned: a partial answer from a turn that failed is not an answer,
        // and every other surface (ACP, the CI runner) already treats a
        // `SessionEvent::Error` as fatal to the turn.
        if stopped.is_none() {
            if let Some(message) = failure {
                return Err(CallError::Infrastructure(anyhow::anyhow!(message)));
            }
        }
        let rounds_ran_out = self.state.kernel.as_ref().is_some_and(|snapshot| {
            snapshot.context.fact(MAX_ROUNDS_REACHED_FACT) == Some(&serde_json::json!(true))
        });
        let conclusion = stopped.unwrap_or(if rounds_ran_out {
            RunConclusion::BudgetExhausted
        } else {
            RunConclusion::Success
        });
        Ok(RunOutcome {
            conclusion,
            reply,
            usage,
        })
    }
}

/// How long a stopped run is given to report what it had before it is left.
const WIND_DOWN: Duration = Duration::from_secs(5);

/// Stops a run: interrupts the model call in flight, tells the machine the
/// work is cancelled, and starts the wind-down clock.
async fn stop(
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

/// Per-turn overrides that do not belong to the agent's durable state.
#[derive(Default)]
struct TurnOptions {
    /// System prompt for this turn.
    role: Option<String>,
    /// Whether the model is denied tools outright.
    no_tools: bool,
    /// Domain facts the machine needs in order to run the turn.
    facts: serde_json::Map<String, serde_json::Value>,
}

/// Reads `answer` as `T`, describing the failure in terms a model can act on.
///
/// Tolerates a fenced code block around the JSON, because a model told to
/// answer with JSON very often answers with JSON in a fence, and rejecting that
/// spends a repair attempt on punctuation.
fn parse_candidate<T: DeserializeOwned>(answer: &str) -> Result<T, String> {
    let trimmed = strip_fence(answer.trim());
    if trimmed.is_empty() {
        return Err("the answer was empty".to_string());
    }
    serde_json::from_str::<T>(trimmed).map_err(|e| e.to_string())
}

/// Returns `text` without a surrounding ```/```json fence, if it has one.
fn strip_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim_start_matches(['\r', '\n'])
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
}
