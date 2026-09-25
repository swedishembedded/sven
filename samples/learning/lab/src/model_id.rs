// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! Which model an arm actually measured.
//!
//! This module exists because of a measured failure, not a hypothetical one.
//! The same checkpoint is reachable through two different serving paths, and
//! only one of them ever receives a trained adapter:
//!
//! * the **resident**, addressed by its manifest id (`brain/qwen3`), which is
//!   what the adapter watcher applies a promoted adapter to; and
//! * the **model store**, addressed by vendor and repo (`Qwen/Qwen3-0.6B`),
//!   which is served by the base weights and keeps being served by the base
//!   weights after every promotion.
//!
//! Both answer requests. Neither errors. An experiment pointed at the second
//! one measures the base model in both of its arms and reports that learning
//! achieved nothing - which is indistinguishable from a real null result, and
//! was in fact observed twice before the cause was found.
//!
//! The second trap is on sven's side: `SVEN_MODEL` splits on the FIRST slash
//! into provider and model name, so the resident id needs the provider spelled
//! in front of it - `brain/brain/qwen3`. The single-prefix `brain/qwen3` sends
//! the name `qwen3`, which no server resolves.
//!
//! Every arm records the [`ServedModel`] it ran against, so a number can never
//! be silently attributed to the wrong weights.

use std::fmt;

/// A model as the serving side names it, with the spellings each consumer
/// needs derived rather than written out by hand at each call site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedModel {
    manifest_id: String,
}

impl ServedModel {
    /// A resident, named by the manifest id the server reports in
    /// `/v1/models` and in its own adapter-watcher line (`brain/qwen3`).
    ///
    /// This is the only constructor on purpose. A model-store spelling
    /// (`Qwen/Qwen3-0.6B`) is not a resident and cannot receive an adapter, so
    /// there is deliberately no way to build a `ServedModel` from one.
    pub fn resident(manifest_id: impl Into<String>) -> ServedModel {
        ServedModel {
            manifest_id: manifest_id.into(),
        }
    }

    /// Qwen3 as brain's resident serves it. The size is not part of the id:
    /// which checkpoint is resident is decided by the server's own
    /// configuration, which is why an arm records the [`ServedModel`] *and*
    /// the checkpoint the server reported.
    pub fn qwen3() -> ServedModel {
        ServedModel::resident("brain/qwen3")
    }

    /// The id to put in an OpenAI-compatible `model` field.
    pub fn api_model(&self) -> &str {
        &self.manifest_id
    }

    /// The provider sven routes this model through.
    ///
    /// Needed because `SVEN_MODEL` is a command-line argument of the `sven`
    /// binary, not something `sven_sdk::config::load` reads: an application
    /// embedding the SDK has to put the provider and the name on the `Config`
    /// itself. Which is the better default for an experiment anyway - an arm
    /// whose weights were chosen by auto-detection is not a controlled arm.
    pub fn provider(&self) -> &'static str {
        "brain"
    }

    /// The value for sven's `SVEN_MODEL`, which is `<provider>/<name>` split
    /// on the first slash - so a resident id that already contains a slash
    /// gets the provider spelled in front of it.
    pub fn sven_model(&self) -> String {
        format!("brain/{}", self.manifest_id)
    }
}

impl fmt::Display for ServedModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.manifest_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_model_is_the_resident_manifest_id_not_the_store_spelling() {
        // Measured: a request for the store spelling is answered by the base
        // weights forever, whatever has been promoted.
        assert_eq!(ServedModel::qwen3().api_model(), "brain/qwen3");
        assert_ne!(ServedModel::qwen3().api_model(), "Qwen/Qwen3-0.6B");
    }

    #[test]
    fn sven_model_carries_the_provider_in_front_of_a_slashed_id() {
        // SVEN_MODEL splits on the first slash: "brain/qwen3" would send the
        // model name "qwen3" and fail the turn.
        assert_eq!(ServedModel::qwen3().sven_model(), "brain/brain/qwen3");
    }

    #[test]
    fn a_resident_id_without_a_slash_still_round_trips() {
        let m = ServedModel::resident("lfm2");
        assert_eq!(m.api_model(), "lfm2");
        assert_eq!(m.sven_model(), "brain/lfm2");
    }
}
