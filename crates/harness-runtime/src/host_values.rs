// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What the host provides a run, readable in its templates as `run.<name>`.
//!
//! The run's private directory (`run.scratchpad`) and the file the host writes
//! its trajectory to (`run.trajectory_file`) are decided outside the machine.
//! They reach the run once, as the `run` fact of its state when it starts, so
//! a checkpoint carries them and a resumed run reads the same values. A host
//! that resumes into another place patches the snapshot before the machine
//! restores it ([`apply_to_snapshot`]).
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in keeping the files of an
//! automated run out of the repository it works on then you can procure our
//! services by sending an email to info@swedishembedded.com.

use serde_json::{json, Map, Value};
use sven_harness_spec::{HarnessPlan, Template};
use sven_hsm::Snapshot;

use crate::run::{HarnessRunData, HarnessRunState};

/// The fact the host-provided values of the run live under (`run.*`).
pub(crate) const RUN_FACT: &str = "run";
pub(crate) const TRAJECTORY_FILE: &str = "trajectory_file";
const SCRATCHPAD: &str = "scratchpad";

/// The values a host provides a run; each is absent until the host gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunValues {
    /// The run's private directory.
    pub scratchpad: Option<String>,
    /// The file the host writes the run's trajectory to before a gate that
    /// reads it runs.
    pub trajectory_file: Option<String>,
}

impl RunValues {
    fn from_fact(fact: Option<&Value>) -> Self {
        let text = |key: &str| {
            fact.and_then(|f| f.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Self {
            scratchpad: text(SCRATCHPAD),
            trajectory_file: text(TRAJECTORY_FILE),
        }
    }

    fn to_fact(&self) -> Value {
        let mut fact = Map::new();
        for (key, value) in [
            (SCRATCHPAD, &self.scratchpad),
            (TRAJECTORY_FILE, &self.trajectory_file),
        ] {
            if let Some(text) = value {
                fact.insert(key.to_string(), json!(text));
            }
        }
        Value::Object(fact)
    }

    /// These values, with every value `over` has replacing the one here.
    #[must_use]
    pub fn overlaid(mut self, over: &RunValues) -> Self {
        if over.scratchpad.is_some() {
            self.scratchpad.clone_from(&over.scratchpad);
        }
        if over.trajectory_file.is_some() {
            self.trajectory_file.clone_from(&over.trajectory_file);
        }
        self
    }
}

impl HarnessRunState {
    /// The values the host provided this run.
    #[must_use]
    pub fn run_values(&self) -> RunValues {
        RunValues::from_fact(self.facts.get(RUN_FACT))
    }

    pub(crate) fn set_run_values(&mut self, values: &RunValues) {
        self.facts.insert(RUN_FACT.to_string(), values.to_fact());
    }
}

/// Lays `values` over the ones a snapshot's run state records: how a host that
/// resumes a run puts it in the directory it chose now rather than the one the
/// checkpoint names.
pub fn apply_to_snapshot(snapshot: &mut Snapshot, values: &RunValues) {
    let mut state = HarnessRunState::load(&snapshot.context);
    let merged = state.run_values().overlaid(values);
    state.set_run_values(&merged);
    state.store(&mut snapshot.context);
}

impl HarnessRunData {
    /// Names the file the host writes the run's trajectory to before a gate
    /// that reads `{{ run.trajectory_file }}` runs.
    #[must_use]
    pub fn with_trajectory_file(mut self, file: impl Into<String>) -> Self {
        self.host.trajectory_file = Some(file.into());
        self
    }

    /// Names the run's private directory (`{{ run.scratchpad }}`). The host
    /// creates it before the run starts.
    #[must_use]
    pub fn with_scratchpad(mut self, dir: impl Into<String>) -> Self {
        self.host.scratchpad = Some(dir.into());
        self
    }

    /// Names everything the host provides the run at once.
    #[must_use]
    pub fn with_run_values(mut self, values: RunValues) -> Self {
        self.host = values;
        self
    }

    /// The values the host named for this run.
    #[must_use]
    pub fn run_values(&self) -> &RunValues {
        &self.host
    }

    /// The scratchpad the document names (`execution.scratchpad`), rendered
    /// over this run's inputs; `None` when it names none.
    ///
    /// # Errors
    ///
    /// The template reads an input the run was not given.
    pub fn document_scratchpad(&self) -> Result<Option<String>, String> {
        let Some(text) = self.plan().scratchpad.as_deref() else {
            return Ok(None);
        };
        let template = Template::parse(text).map_err(|e| format!("execution.scratchpad: {e}"))?;
        let state = HarnessRunState {
            inputs: self.inputs().clone(),
            ..HarnessRunState::default()
        };
        if let Some(missing) = template
            .paths()
            .into_iter()
            .find(|path| state.resolve_path(path).is_none())
        {
            return Err(format!(
                "execution.scratchpad reads `{missing}`, which this run was not given"
            ));
        }
        let rendered = template.render(&state.resolver());
        Ok(Some(rendered))
    }
}

/// `true` when a template of `plan` reads `{{ run.scratchpad }}`: the host
/// then has to provide the directory.
#[must_use]
pub fn plan_reads_scratchpad(plan: &HarnessPlan) -> bool {
    // Any step may read it (prompts, system texts, gates), so the check is
    // over the whole plan rather than over one kind of step.
    serde_json::to_string(&plan.steps)
        .is_ok_and(|text| text.contains(&format!("facts.{RUN_FACT}.{SCRATCHPAD}")))
}
