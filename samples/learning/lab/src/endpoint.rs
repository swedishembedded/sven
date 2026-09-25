// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! Proof that an endpoint can actually be learned on.
//!
//! The expensive failure this module removes is not a crash. It is a run that
//! completes, reports a plausible number, and measured the wrong weights: an
//! adapter is promoted, every request is still answered by the base model, and
//! the before/after table shows no difference. That reads as "training
//! achieved nothing" and sends whoever hits it to debug the trainer.
//!
//! It happened twice while this harness was being built, for two different
//! reasons - a server holding no resident to apply adapters to, and a model id
//! naming a serving path that never receives them - and both times the
//! *symptom* was a perfectly well-formed null result.
//!
//! A warning would not have helped; there was a warning available in the
//! second case and it scrolled past in a log. So scoring does not take a URL.
//! It takes an [`AdapterPath`], which cannot be constructed except from the
//! server's own statement that it is watching a directory of adapters for a
//! named model. An arm scored against a server that cannot receive adapters is
//! not a bug to notice in review - it does not compile.

use std::fmt;

use crate::ServedModel;

/// Evidence that promoted adapters reach a specific model on a specific
/// server.
///
/// The only constructor parses the line `brain serve` prints when its adapter
/// watcher starts. That line is emitted *after* the watcher has a resident to
/// apply an adapter to, so its presence is the server's own report that both
/// halves are in place - which is exactly the thing that cannot be inferred
/// from a successful request, because requests succeed either way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterPath {
    model: ServedModel,
    watching: String,
}

/// Why an endpoint could not be shown to apply adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoAdapterPath {
    /// The server never reported starting a watcher. Either `--watch-adapters`
    /// was not given, or it was given with no resident to apply an adapter to
    /// and was ignored.
    NoWatcher,
    /// A watcher is running, but for a different model than the one about to
    /// be measured. This is the trap that produces a convincing null result:
    /// both the watched model and the requested one answer normally.
    WatchingAnotherModel { watching: String, requested: String },
}

impl fmt::Display for NoAdapterPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoAdapterPath::NoWatcher => f.write_str(
                "the server reported no adapter watcher, so a promoted adapter would never take \
                 effect and both arms would measure the base weights. Serve with \
                 --watch-adapters DIR and name the checkpoint (BRAIN_QWEN_WEIGHTS) so the \
                 process holds a resident to apply it to.",
            ),
            NoAdapterPath::WatchingAnotherModel {
                watching,
                requested,
            } => write!(
                f,
                "the adapter watcher is running for {watching:?} but this arm would measure \
                 {requested:?}. Both answer requests, and only {watching:?} receives adapters, so \
                 the comparison would silently measure base weights in both arms.",
            ),
        }
    }
}

impl AdapterPath {
    /// The marker `brain serve` prints once its watcher is live.
    const WATCHING: &'static str = "watching ";
    const FOR_ADAPTERS: &'static str = " for promoted LoRA adapters";

    /// Read the server's own log for its adapter-watcher line and confirm it
    /// names the model about to be measured.
    ///
    /// Takes the whole log rather than one line because the caller has the log
    /// and should not have to know which line matters.
    pub fn from_server_log(log: &str, model: &ServedModel) -> Result<AdapterPath, NoAdapterPath> {
        let line = log
            .lines()
            .find(|l| l.contains(Self::WATCHING) && l.contains(Self::FOR_ADAPTERS))
            .ok_or(NoAdapterPath::NoWatcher)?;

        // `... watching <dir> for promoted LoRA adapters (<model>)`
        let watching = line
            .rsplit_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(id, _)| id.trim().to_string())
            .ok_or(NoAdapterPath::NoWatcher)?;

        if watching != model.api_model() {
            return Err(NoAdapterPath::WatchingAnotherModel {
                watching,
                requested: model.api_model().to_string(),
            });
        }
        Ok(AdapterPath {
            model: model.clone(),
            watching,
        })
    }

    /// The model whose weights a promoted adapter will actually change.
    pub fn model(&self) -> &ServedModel {
        &self.model
    }

    /// The model id the server itself named, for the run record.
    pub fn watching(&self) -> &str {
        &self.watching
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic directories: the parser cares about the shape of the line and
    // the model id in the parentheses, never about where the directory is.
    const REAL: &str = "brain serve: scanning model dir <models-dir>\n\
         brain serve: watching <adapters-dir> for promoted LoRA adapters (brain/qwen3)\n\
         brain serve: ready\n";

    #[test]
    fn a_watcher_for_this_model_is_the_proof() {
        let path = AdapterPath::from_server_log(REAL, &ServedModel::qwen3()).expect("proof");
        assert_eq!(path.watching(), "brain/qwen3");
    }

    #[test]
    fn a_server_with_no_watcher_cannot_be_measured_on() {
        // The observed failure: --watch-adapters accepted, nothing logged,
        // every request answered normally, no swap ever performed.
        let quiet = "brain serve: scanning model dir\nbrain serve: ready\n";
        assert_eq!(
            AdapterPath::from_server_log(quiet, &ServedModel::qwen3()),
            Err(NoAdapterPath::NoWatcher)
        );
    }

    #[test]
    fn a_watcher_for_a_different_model_is_refused_and_names_both() {
        // The second observed failure: the watcher applies adapters to the
        // resident while the probes ask for the model-store spelling.
        let err = AdapterPath::from_server_log(REAL, &ServedModel::resident("Qwen/Qwen3-0.6B"))
            .expect_err("a different model must not count as proof");
        assert_eq!(
            err,
            NoAdapterPath::WatchingAnotherModel {
                watching: "brain/qwen3".into(),
                requested: "Qwen/Qwen3-0.6B".into(),
            }
        );
        let text = err.to_string();
        assert!(text.contains("brain/qwen3") && text.contains("Qwen/Qwen3-0.6B"));
    }

    #[test]
    fn the_explanation_says_what_would_have_gone_wrong_not_just_what_is_missing() {
        // A message that only says "no watcher" invites the reader to treat it
        // as noise. It has to say that the RESULT would have been wrong.
        let text = NoAdapterPath::NoWatcher.to_string();
        assert!(text.contains("base weights"), "{text}");
        assert!(text.contains("BRAIN_QWEN_WEIGHTS"), "{text}");
    }
}
