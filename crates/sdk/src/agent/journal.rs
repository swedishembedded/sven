// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements crash-robust agent telemetry for its clients.
// If your team needs expertise in keeping agent run records intact when
// machines fail then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Writing an agent's trajectory as the run happens.
//!
//! [`Agent::trajectory`] is the document after the fact; a process killed in
//! the middle of a run never gets to write it. The journal appends each step to
//! a file as soon as the step is settled, so what a crashed run did is on disk
//! up to its last settled step, and the file says it is incomplete until the
//! journal is closed. See [`atif::persist`] for the file format.

use std::path::Path;

use atif::persist::{FlushPolicy, JournalLimits, JournalWriter};

use super::Agent;
use crate::error::CallError;
use crate::meter::Metered;
use crate::state::CompactionMark;

/// An open journal and how much of the trajectory it already holds.
pub(super) struct Journal {
    writer: Option<JournalWriter>,
    /// Steps of the trajectory already written.
    written: usize,
    /// Why journaling stopped, if it did.
    failed: Option<String>,
}

impl Agent {
    /// Starts writing this agent's trajectory to a new journal at `path`: the
    /// steps already in it now, every later step as it settles. The file must
    /// not exist. Call [`Self::close_journal`] when the agent is done; a
    /// journal that was never closed reads back as incomplete.
    ///
    /// A write that fails (disk full, a bound in `limits`) stops the journal,
    /// raises a [`SessionEvent::Warning`](crate::SessionEvent::Warning) and is
    /// reported again by [`Self::journal_error`] and [`Self::close_journal`];
    /// it never fails the run.
    ///
    /// # Errors
    ///
    /// [`CallError::Infrastructure`] when the file cannot be created.
    pub fn journal_to(
        &mut self,
        path: &Path,
        policy: FlushPolicy,
        limits: JournalLimits,
    ) -> Result<(), CallError> {
        let header = self.trajectory();
        let writer = JournalWriter::create(path, &header, policy, limits).map_err(|e| {
            CallError::Infrastructure(anyhow::anyhow!("journal {}: {e}", path.display()))
        })?;
        self.journal = Some(Journal {
            writer: Some(writer),
            written: 0,
            failed: None,
        });
        if let Some(why) = self.flush_journal(&[], &[], true) {
            return Err(CallError::Infrastructure(anyhow::anyhow!("journal: {why}")));
        }
        Ok(())
    }

    /// Why the journal stopped, if it did.
    #[must_use]
    pub fn journal_error(&self) -> Option<&str> {
        self.journal.as_ref()?.failed.as_deref()
    }

    /// Writes the rest of the trajectory and the end record, and closes the
    /// journal: the run finished. A no-op without a journal.
    ///
    /// # Errors
    ///
    /// [`CallError::Infrastructure`] when the journal had stopped earlier or the
    /// end record cannot be written; the file then stays incomplete.
    pub fn close_journal(&mut self) -> Result<(), CallError> {
        self.flush_journal(&[], &[], true);
        let Some(mut journal) = self.journal.take() else {
            return Ok(());
        };
        if let Some(why) = journal.failed.take() {
            return Err(CallError::Infrastructure(anyhow::anyhow!(
                "journal stopped: {why}"
            )));
        }
        let Some(writer) = journal.writer.take() else {
            return Ok(());
        };
        let totals = self.trajectory().final_metrics;
        writer
            .finish(totals.as_ref())
            .map_err(|e| CallError::Infrastructure(anyhow::anyhow!("journal end record: {e}")))
    }

    /// Appends the steps of the trajectory that are settled and not yet
    /// written. `running_requests` and `running_marks` are what the run in
    /// progress has folded but not yet handed to the state. When `all` is
    /// false the last step is held back: it may still grow. Returns why the
    /// journal stopped, when this flush is what stopped it.
    pub(super) fn flush_journal(
        &mut self,
        running_requests: &[Metered],
        running_marks: &[CompactionMark],
        all: bool,
    ) -> Option<String> {
        if self.journal.as_ref().is_none_or(|j| j.writer.is_none()) {
            return None;
        }
        let steps = self.trajectory_with(running_requests, running_marks).steps;
        let settled = if all {
            steps.len()
        } else {
            steps.len().saturating_sub(1)
        };
        let journal = self.journal.as_mut()?;
        let writer = journal.writer.as_mut()?;
        let mut failure = None;
        for step in steps.iter().take(settled).skip(journal.written) {
            if let Err(e) = writer.append_step(step) {
                failure = Some(e.to_string());
                break;
            }
            journal.written += 1;
        }
        let why = failure?;
        journal.failed = Some(why.clone());
        journal.writer = None;
        Some(why)
    }
}
