// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What a decision provider reports about the models it can serve, and how a
//! configured model is turned into one that is ready to use.

use crate::{DecisionError, QuestionKind};

/// Whether a model can be served right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelState {
    Ready,
    /// Its files are not on the provider.
    NotInstalled,
    /// Its files are there but are not a servable decision model.
    Incompatible,
}

impl ModelState {
    /// The name used in diagnostics.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NotInstalled => "not installed",
            Self::Incompatible => "incompatible",
        }
    }
}

/// One model as the provider reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub state: ModelState,
    pub backend: Option<String>,
    /// A validation is recorded for the model's weights.
    pub validated: bool,
    /// The task profile the validation was measured on.
    pub validation_profile: Option<String>,
    /// Identity of the weights, when the provider declares it.
    pub checkpoint: Option<String>,
    /// The most input tokens the model holds, when it declares it.
    pub max_input_tokens: Option<u32>,
    pub question_types: Vec<QuestionKind>,
    /// The provider's own remedy for a model that is not installed.
    pub hint: Option<String>,
    /// Why a model is incompatible.
    pub detail: Option<String>,
}

/// The provider's answer to "which model would `auto` use".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub chosen: Option<String>,
    pub rule: String,
    pub reason: String,
}

/// How good a model must be to be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MinimumQuality {
    /// Any model that can answer.
    #[default]
    Any,
    /// Only a model with a recorded validation.
    Validated,
}

/// The id that asks the provider to choose the model.
pub const AUTO: &str = "auto";

/// The model `requested` names, ready to use, among the provider's `models`.
/// `selection` is the provider's choice and is consulted only for
/// [`AUTO`].
///
/// # Errors
///
/// The class that says why no model can be used: not installed, failed to
/// load (incompatible), or unsupported (not validated while validation is
/// required).
pub fn resolve_model(
    requested: &str,
    models: &[ModelInfo],
    selection: Option<&Selection>,
    quality: MinimumQuality,
) -> Result<ModelInfo, DecisionError> {
    let id = if requested == AUTO {
        let chosen = selection.and_then(|s| s.chosen.as_deref());
        match chosen {
            Some(id) => id.to_string(),
            None => return Err(nothing_selected(models, selection)),
        }
    } else {
        requested.to_string()
    };
    let model =
        models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| DecisionError::ModelNotInstalled {
                model: id.clone(),
                detail: "the provider does not know this model".into(),
            })?;
    match model.state {
        ModelState::NotInstalled => Err(DecisionError::ModelNotInstalled {
            model: id,
            detail: model
                .hint
                .clone()
                .unwrap_or_else(|| "its files are missing".into()),
        }),
        ModelState::Incompatible => Err(DecisionError::FailedToLoad {
            model: id,
            detail: model
                .detail
                .clone()
                .unwrap_or_else(|| "its files are not a servable decision model".into()),
        }),
        ModelState::Ready if quality == MinimumQuality::Validated && !model.validated => {
            Err(DecisionError::UnsupportedCapability(format!(
                "decision model '{id}' has no recorded validation and the minimum model \
                 quality is validated"
            )))
        }
        ModelState::Ready => Ok(model.clone()),
    }
}

/// Why `auto` selected nothing: models that are installed but unvalidated
/// are a different problem, with a different remedy, from none installed.
fn nothing_selected(models: &[ModelInfo], selection: Option<&Selection>) -> DecisionError {
    let unvalidated: Vec<&str> = models
        .iter()
        .filter(|m| m.state == ModelState::Ready && !m.validated)
        .map(|m| m.id.as_str())
        .collect();
    if !unvalidated.is_empty() {
        return DecisionError::UnsupportedCapability(format!(
            "no installed decision model has a recorded validation ({} installed without one)",
            unvalidated.join(", ")
        ));
    }
    let reason = selection.map_or("no decision model is available", |s| s.reason.as_str());
    let remedies: Vec<&str> = models
        .iter()
        .filter(|m| m.state == ModelState::NotInstalled)
        .filter_map(|m| m.hint.as_deref())
        .take(3)
        .collect();
    let detail = if remedies.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}; {}", remedies.join("; "))
    };
    DecisionError::ModelNotInstalled {
        model: AUTO.into(),
        detail,
    }
}
