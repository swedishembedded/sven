// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Child runs: how a parent loop starts them, what they inherit, and how
//! they are stopped with it.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use sven_hsm::{
    ChildRunContract, Effect, Event, InternalEvent, MachineId, ObservationSink, PermissionPolicy,
    StateLabel,
};

use crate::{CancelScope, EffectExecutor, EventSink};

/// What a child run is started with: the terms it runs under and the handle
/// that stops it.
///
/// `cancel` is derived from the parent run's scope, so cancelling or shutting
/// down the parent stops the child. The kernel builds `contract` from the
/// capabilities the parent holds in the state that spawned the child, narrowed
/// by the parent's own contract when it has one; a spawner may narrow it
/// further but must never widen it.
#[derive(Clone, Debug)]
pub struct ChildRun {
    /// The capabilities, budgets and deadline the child runs under.
    pub contract: ChildRunContract,
    /// Stops the child. Cancelled when the parent is cancelled or shut down.
    pub cancel: CancelScope,
}

/// Spawns child submachines in response to [`Effect::InstantiateSubmachine`].
///
/// The kernel itself is machine-agnostic, so it cannot build a concrete child
/// from an opaque descriptor. A `ChildSpawner` bridges that gap: given the
/// parent-assigned [`MachineId`], the descriptor, the [`ChildRun`] terms and a
/// clone of the parent's [`EventSink`], it must run the child **concurrently
/// on its own task with an isolated [`Context`](sven_hsm::Context)** under
/// `run.contract`, stop it when `run.cancel` is cancelled, and, when the child
/// ends, post `Event::Internal(InternalEvent::SubmachineCompleted { machine,
/// result })` back to the parent so the parent can aggregate the result
/// (append-only). [`ErasedRuntime::spawn_child_run`](crate::ErasedRuntime::spawn_child_run)
/// does the enforcing part for an in-process child.
///
/// `spawn_child` should return promptly (spawn-and-forget); long-running child
/// work belongs on the task it spawns, never inline, so the parent's single
/// consumer loop is never blocked. This is what makes fan-out *parallel*.
#[async_trait]
pub trait ChildSpawner: Send + Sync {
    /// Builds and starts the child identified by `machine`.
    ///
    /// Implementations post the terminal [`InternalEvent::SubmachineCompleted`]
    /// (carrying the child's result payload) into `parent` when done.
    async fn spawn_child(
        &self,
        machine: MachineId,
        descriptor: Value,
        run: ChildRun,
        parent: EventSink,
    );
}

/// The child runs in flight for a parent loop, and the spawner that starts
/// them.
///
/// The parent machine drives aggregation (it owns the append-only thread), so
/// the kernel keeps only what it needs to stop children: each live child's
/// cancel scope, inserted on spawn and removed on
/// [`InternalEvent::SubmachineCompleted`]. Dropping the registry - the parent
/// loop ending for any reason, including an abort - cancels every child
/// still live, so no child outlives the run that started it.
pub(crate) struct Children {
    spawner: Option<Arc<dyn ChildSpawner>>,
    /// The parent run's own scope; every child scope derives from it.
    scope: CancelScope,
    /// The parent run's own contract, when it is itself a child: what it
    /// holds bounds what it can hand down.
    terms: Option<ChildRunContract>,
    live: HashMap<MachineId, CancelScope>,
    /// The parent run's own deadline, armed for as long as the run lives.
    deadline: Option<crate::DeadlineTimer>,
}

impl Children {
    pub(crate) fn new(
        spawner: Option<Arc<dyn ChildSpawner>>,
        scope: CancelScope,
        terms: Option<ChildRunContract>,
    ) -> Self {
        Self {
            spawner,
            scope,
            terms,
            live: HashMap::new(),
            deadline: None,
        }
    }

    /// Holds the run's deadline timer, so it is disarmed when the run ends.
    pub(crate) fn with_deadline(mut self, timer: Option<crate::DeadlineTimer>) -> Self {
        self.deadline = timer;
        self
    }

    /// Runs one effect the permission gate already allowed: an
    /// [`Effect::InstantiateSubmachine`] goes to the spawner when one is
    /// configured, with the contract the parent holds in `state`; everything
    /// else goes to `executor` (which answers an instantiate it cannot serve
    /// with a failure event).
    pub(crate) async fn execute<E: EffectExecutor>(
        &mut self,
        effect: Effect,
        policy: &PermissionPolicy,
        state: &StateLabel,
        executor: &mut E,
        sink: &EventSink,
        obs: &ObservationSink,
    ) {
        match (effect, &self.spawner) {
            (
                Effect::InstantiateSubmachine {
                    machine,
                    descriptor,
                },
                Some(spawner),
            ) => {
                let inherited = ChildRunContract::inherit(policy, state);
                let contract = match &self.terms {
                    Some(terms) => inherited.narrow(terms),
                    None => inherited,
                };
                let cancel = self.scope.child();
                self.live.insert(machine, cancel.clone());
                let run = ChildRun { contract, cancel };
                spawner
                    .spawn_child(machine, descriptor, run, sink.clone())
                    .await;
            }
            (effect, _) => executor.execute(effect, sink, obs).await,
        }
    }

    /// The scope of the run this registry belongs to.
    pub(crate) fn scope(&self) -> CancelScope {
        self.scope.clone()
    }

    /// Drops a completed child from the registry.
    pub(crate) fn note_completion(&mut self, event: &Event) {
        if let Event::Internal(InternalEvent::SubmachineCompleted { machine, .. }) = event {
            if let Ok(uuid) = uuid::Uuid::parse_str(machine) {
                self.live.remove(&MachineId::from_uuid(uuid));
            }
        }
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        for child in self.live.values() {
            child.cancel();
        }
    }
}
