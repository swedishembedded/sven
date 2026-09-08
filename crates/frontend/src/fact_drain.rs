// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Background task that drains the pending-facts ledger towards training.
//!
//! [`sven_memory::PendingFactsDrain`] owns the crash-safe protocol; this module
//! owns only the two questions the wiring tier is allowed to answer: *may this
//! run at all* (a config flag the user set) and *when* (a periodic task).
//!
//! # Why here and not in the kernel
//!
//! The HSM must not own a training pipeline. Its transitions are pure and its
//! `Effect`s are the audited things a *turn* does; a drain that ticks on a
//! timer, outlives every session and talks to a remote scheduler is none of
//! that. So it is a plain `tokio` task in the wiring tier, alongside the other
//! background tasks every frontend shares, and the kernel never learns it
//! exists.
//!
//! # Why the flag is a consent boundary
//!
//! This task is the one place where a fact the user stated in private leaves
//! the machine. `tools.memory.learning.submit_facts` therefore defaults to
//! off, an absent config section means off, and "off" means no task at all -
//! not a task that runs and finds nothing to do. The concrete
//! [`FactSubmitter`] is injected by the caller (whale's is the real one), so
//! this crate never learns where the facts go either.
//!
//! # Why only one drain may run
//!
//! [`sven_memory::PendingFactsDrain`]'s exactly-once guarantee rests on a
//! cursor file with a single owner, and the ledger lives at one well-known
//! path per machine - so two `sven` processes open at once would otherwise
//! each spawn a drain over the same cursor and submit the same batch twice,
//! which is precisely the double-training the ledger exists to prevent.
//! Spawning therefore takes an exclusive advisory lock on a sidecar beside
//! the cursor (the same mechanism [`sven_chain`] uses for the ledger itself)
//! and declines to start when another process already holds it. The lock is
//! released when the task ends or the process exits.
//!
//! Swedish Embedded AB implements solutions for consent-gated knowledge
//! pipelines in autonomous agents for its clients. If your team needs
//! expertise in background task supervision for agent runtimes then you can
//! procure our services by sending an email to info@swedishembedded.com.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sven_config::Config;
use sven_memory::{FactOutcome, FactSubmitter, PendingFactsDrain, PendingFactsLedger};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Spawns the pending-facts drain with the submitter `config` selects.
///
/// This is the ordinary entry point: a frontend has no opinion about where
/// facts go, and the selection is a config value
/// (`tools.memory.learning.submitter`, `local` by default) so that a later
/// remote submitter can be swapped in without the drain - generic over
/// [`FactSubmitter`] precisely for this - changing at all. Use
/// [`spawn_fact_drain`] directly to inject a submitter, which is what the
/// tests do.
///
/// Returns `None` and spawns nothing whenever [`spawn_fact_drain`] would, and
/// additionally when no submitter is configured or the configured one cannot
/// be built - a misconfigured submitter is logged and skipped rather than
/// taken as a reason to fail the session it is a background task of.
pub fn spawn_default_fact_drain(
    config: &Config,
    ledger: PendingFactsLedger,
) -> Option<JoinHandle<()>> {
    // Checked before building anything: an unset flag means no submitter is
    // constructed at all, so a machine that never opted in is never even asked
    // for an `adapter_dir`.
    if !config.tools.memory.learning.submit_facts {
        return None;
    }
    match sven_memory::submitter_from_config(config) {
        Ok(Some(submitter)) => spawn_fact_drain(config, ledger, submitter),
        Ok(None) => {
            info!("pending-facts drain: no submitter configured; facts stay in the ledger");
            None
        }
        Err(e) => {
            warn!(error = %e, "pending-facts drain: submitter is misconfigured; not started");
            None
        }
    }
}

/// Spawns the pending-facts drain, if the user asked for it.
///
/// Returns `None` - and spawns nothing - unless
/// `tools.memory.learning.submit_facts` is set and this process can claim the
/// ledger's single-drain lock. `ledger` must be the same ledger the
/// `assimilate_fact`/`ingest_document` tools write to
/// ([`PendingFactsLedger::at_default_path`] is what `RuntimeBuilder` hands
/// them); the drain's own cursor is persisted beside it.
///
/// The returned handle is the caller's to abort. Dropping it leaves the task
/// running for the life of the process, which is the intended behaviour for a
/// frontend that keeps its session open.
pub fn spawn_fact_drain(
    config: &Config,
    ledger: PendingFactsLedger,
    submitter: Arc<dyn FactSubmitter>,
) -> Option<JoinHandle<()>> {
    let settings = &config.tools.memory.learning;
    if !settings.submit_facts {
        return None;
    }
    let cursor_path = PendingFactsDrain::default_cursor_path(&ledger);
    let lock = match claim_sole_drain(&cursor_path) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            info!(
                cursor = %cursor_path.display(),
                "pending-facts drain already running elsewhere; not starting a second one"
            );
            return None;
        }
        // Never start unlocked: an unguarded second drain double-submits, and
        // not draining costs only a delay.
        Err(e) => {
            warn!(
                cursor = %cursor_path.display(),
                error = %e,
                "cannot claim the pending-facts drain lock; drain not started"
            );
            return None;
        }
    };
    let drain = PendingFactsDrain::new(ledger.clone(), &cursor_path, settings.batch_size);
    // A zero interval would spin the drain against the disk as fast as the
    // runtime allows; one second is the floor, not the default.
    let interval = Duration::from_secs(settings.interval_secs.max(1));
    info!(
        ledger = %ledger.path().display(),
        batch_size = settings.batch_size,
        interval_secs = interval.as_secs(),
        "pending-facts drain enabled: admitted facts will be submitted for training"
    );
    Some(tokio::spawn(drain_loop(drain, interval, submitter, lock)))
}

/// Takes the exclusive advisory lock on `<cursor>.lock`.
///
/// `Ok(None)` means another live drain holds it - the caller must not start a
/// second one. The returned file must outlive the drain: dropping it releases
/// the lock.
fn claim_sole_drain(cursor_path: &Path) -> std::io::Result<Option<File>> {
    let mut os = cursor_path.as_os_str().to_owned();
    os.push(".lock");
    let path = PathBuf::from(os);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Drains on a fixed interval, starting immediately.
///
/// A failed pass is logged and retried on the next tick rather than ending the
/// task: the drain's cursor is unchanged by a failure, so retrying is the
/// correct and safe response to every [`sven_memory::DrainError`], including a
/// submitter that is merely unreachable right now.
async fn drain_loop(
    drain: PendingFactsDrain,
    interval: Duration,
    submitter: Arc<dyn FactSubmitter>,
    // Held for the life of the task: this is what stops a second process from
    // draining the same cursor. Released when the task ends or the process
    // exits.
    _sole_drain_lock: File,
) {
    loop {
        match drain.drain_once(submitter.as_ref()).await {
            Ok(reports) if reports.is_empty() => {
                debug!("pending-facts drain: nothing to submit");
            }
            Ok(reports) => {
                for report in reports {
                    match &report.outcome {
                        FactOutcome::Promoted { numbers } => {
                            info!(
                                fact = report.id.as_str(),
                                baseline_pass_rate = ?numbers.baseline_pass_rate,
                                post_training_pass_rate = ?numbers.post_training_pass_rate,
                                "fact promoted into the model"
                            )
                        }
                        FactOutcome::Rejected { reason, .. } => info!(
                            fact = report.id.as_str(),
                            reason = reason.as_str(),
                            "fact was not promoted"
                        ),
                        FactOutcome::Failed { reason } => warn!(
                            fact = report.id.as_str(),
                            reason = reason.as_str(),
                            "fact submission failed"
                        ),
                    }
                }
            }
            Err(e) => warn!(error = %e, "pending-facts drain pass failed; retrying next tick"),
        }
        tokio::time::sleep(interval).await;
    }
}
