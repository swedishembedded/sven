// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The variant matrix: a typed file, validated offline, and the realization
//! of each variant as a configuration file plus a disclosure flag.
//!
//! A variant is realized only through configuration the product already
//! reads and through a [`sven_decision::DecisionProvider`] served to it; there
//! is no benchmark-only code path in the product.

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

/// The only matrix format this build reads.
pub const MATRIX_SCHEMA: &str = "sven.ablation.matrix/1";

/// The config key whose value is the variant's `context_assembly`.
pub const ASSEMBLY_KEY: &str = "context.assembly.mode";

/// Why a matrix is refused, or a variant cannot run.
#[derive(Debug, thiserror::Error)]
pub enum MatrixError {
    #[error("matrix is not valid YAML for this schema: {0}")]
    Parse(String),
    #[error("{0}")]
    Invalid(String),
}

/// Why a variant cannot run in this invocation. Recorded as a
/// `not available` run, never silently dropped.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Unavailable(pub String);

/// What a variant is for in the comparison. The report's attribution table
/// is built from roles, so each role appears at most once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The legacy harness every ratio is measured against.
    Baseline,
    /// The new architecture with decisions off.
    Architecture,
    /// The architecture with a deterministic rule-based provider.
    Heuristic,
    /// A stock decision model.
    StockModel,
    /// A trained decision model.
    TrainedModel,
    /// Valid random decisions: a model must beat this.
    NegativeControl,
    /// The labelled-correct answers: the upper bound.
    Oracle,
}

/// How much of each tool's definition a request carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disclosure {
    Full,
    Deferred,
}

impl Disclosure {
    /// The spelling of `--tool-disclosure`.
    #[must_use]
    pub fn flag_value(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Deferred => "deferred",
        }
    }
}

/// Which model of a Brain server a variant asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainModel {
    /// `--stock-model`, `auto` unless given.
    Stock,
    /// `--trained-model`, required.
    Trained,
}

/// Who answers the decisions of a variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decisions {
    /// Decisions are off; nothing is contacted.
    Off,
    /// The rule-based provider of this crate.
    Heuristic,
    /// The seeded random provider of this crate.
    Random,
    /// The oracle of this crate, which knows the labelled answers.
    Oracle,
    /// A model served by a Brain server at an explicit endpoint.
    Brain { model: BrainModel },
}

impl Decisions {
    /// Whether the provider is served by this crate rather than by Brain.
    #[must_use]
    pub fn is_injected(&self) -> bool {
        matches!(self, Self::Heuristic | Self::Random | Self::Oracle)
    }

    /// The model id an injected provider reports, which the variant's
    /// configuration names.
    #[must_use]
    pub fn injected_model_id(&self) -> Option<&'static str> {
        match self {
            Self::Heuristic => Some("ablation/heuristic"),
            Self::Random => Some("ablation/random"),
            Self::Oracle => Some("ablation/oracle"),
            _ => None,
        }
    }
}

/// One column of the comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub description: String,
    pub tool_disclosure: Disclosure,
    /// The value of `context.assembly.mode`; `off` is the legacy behaviour.
    pub context_assembly: String,
    pub decisions: Decisions,
}

/// A matrix file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matrix {
    pub schema: String,
    pub variants: Vec<Variant>,
}

/// Where the decision providers of a run can be reached.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// The Brain-compatible server this crate runs for injected providers.
    pub local_provider_url: String,
    /// `--brain-url`.
    pub brain_url: Option<String>,
    /// `--stock-model`.
    pub stock_model: String,
    /// `--trained-model`.
    pub trained_model: Option<String>,
}

/// A variant realized: the configuration file and flags that run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realization {
    /// The `--tool-disclosure` value.
    pub tool_disclosure: Disclosure,
    /// A complete configuration file (YAML) for the run.
    pub config_yaml: String,
}

impl Matrix {
    /// Parses and validates a matrix.
    ///
    /// # Errors
    ///
    /// [`MatrixError`] naming the first problem.
    pub fn from_yaml(text: &str) -> Result<Self, MatrixError> {
        let matrix: Self =
            serde_yaml::from_str(text).map_err(|e| MatrixError::Parse(e.to_string()))?;
        matrix.validate()?;
        Ok(matrix)
    }

    /// The baseline variant. Validation guarantees there is exactly one.
    #[must_use]
    pub fn baseline(&self) -> &Variant {
        self.variants
            .iter()
            .find(|v| v.role == Role::Baseline)
            .expect("a validated matrix has a baseline")
    }

    /// The variant with `role`, if the matrix has one.
    #[must_use]
    pub fn by_role(&self, role: Role) -> Option<&Variant> {
        self.variants.iter().find(|v| v.role == role)
    }

    fn validate(&self) -> Result<(), MatrixError> {
        let bad = |m: String| Err(MatrixError::Invalid(m));
        if self.schema != MATRIX_SCHEMA {
            return bad(format!(
                "schema {:?} is not supported; this build reads {MATRIX_SCHEMA:?}",
                self.schema
            ));
        }
        let mut ids: Vec<&str> = Vec::new();
        let mut roles: Vec<Role> = Vec::new();
        for v in &self.variants {
            if !valid_id(&v.id) {
                return bad(format!(
                    "variant id {:?} must be 1 to 16 characters of [A-Za-z0-9_-], starting with a letter",
                    v.id
                ));
            }
            if ids.contains(&v.id.as_str()) {
                return bad(format!("duplicate variant id {:?}", v.id));
            }
            if roles.contains(&v.role) {
                return bad(format!(
                    "role {:?} is used by more than one variant",
                    v.role
                ));
            }
            ids.push(&v.id);
            roles.push(v.role);
            v.validate()?;
        }
        if !roles.contains(&Role::Baseline) {
            return bad("the matrix has no baseline variant (role: baseline)".into());
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && id.len() <= 16
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Variant {
    fn validate(&self) -> Result<(), MatrixError> {
        let bad = |m: String| Err(MatrixError::Invalid(format!("variant {}: {m}", self.id)));
        let expected = match self.role {
            Role::Baseline | Role::Architecture => Some(Decisions::Off),
            Role::Heuristic => Some(Decisions::Heuristic),
            Role::NegativeControl => Some(Decisions::Random),
            Role::Oracle => Some(Decisions::Oracle),
            Role::StockModel => Some(Decisions::Brain {
                model: BrainModel::Stock,
            }),
            Role::TrainedModel => Some(Decisions::Brain {
                model: BrainModel::Trained,
            }),
        };
        if expected.as_ref() != Some(&self.decisions) {
            return bad(format!(
                "role {:?} requires decisions {:?} (the {} provider), found {:?}",
                self.role,
                expected,
                role_provider_word(self.role),
                self.decisions
            ));
        }
        if self.role == Role::Baseline
            && (self.tool_disclosure != Disclosure::Full || self.context_assembly != "off")
        {
            return bad(
                "the baseline is the legacy harness: tool_disclosure full and context_assembly off"
                    .into(),
            );
        }
        if self.role != Role::Baseline && self.context_assembly.is_empty() {
            return bad("context_assembly must be set".into());
        }
        Ok(())
    }

    /// The configuration file and flags that run this variant.
    ///
    /// # Errors
    ///
    /// [`Unavailable`] when the variant needs an endpoint or a model id that
    /// the invocation did not give; the reason names the flag.
    pub fn realize(&self, endpoints: &Endpoints) -> Result<Realization, Unavailable> {
        let mut decision = Mapping::new();
        match &self.decisions {
            Decisions::Off => {
                decision.insert("enabled".into(), false.into());
            }
            injected if injected.is_injected() => {
                set_decision(
                    &mut decision,
                    "required",
                    injected.injected_model_id().unwrap_or_default(),
                    &endpoints.local_provider_url,
                    "any",
                );
            }
            Decisions::Brain { model } => {
                let url = endpoints.brain_url.as_deref().ok_or_else(|| {
                    Unavailable(format!(
                        "not available: variant {} needs a Brain server; pass --brain-url",
                        self.id
                    ))
                })?;
                let model_id = match model {
                    BrainModel::Stock => endpoints.stock_model.clone(),
                    BrainModel::Trained => endpoints.trained_model.clone().ok_or_else(|| {
                        Unavailable(format!(
                            "not available: variant {} evaluates a trained model; pass --trained-model",
                            self.id
                        ))
                    })?,
                };
                set_decision(&mut decision, "auto", &model_id, url, "validated");
            }
            _ => unreachable!("every decisions kind is handled above"),
        }
        let mut config = Mapping::new();
        config.insert("decision".into(), Value::Mapping(decision));
        config.insert(
            "context".into(),
            nested(&["assembly", "mode"], self.context_assembly.clone().into()),
        );
        Ok(Realization {
            tool_disclosure: self.tool_disclosure,
            config_yaml: serde_yaml::to_string(&Value::Mapping(config))
                .expect("a mapping of scalars serializes"),
        })
    }
}

fn role_provider_word(role: Role) -> &'static str {
    match role {
        Role::Baseline | Role::Architecture => "no",
        Role::Heuristic => "heuristic",
        Role::NegativeControl => "random",
        Role::Oracle => "oracle",
        Role::StockModel | Role::TrainedModel => "Brain",
    }
}

fn set_decision(decision: &mut Mapping, mode: &str, model: &str, url: &str, quality: &str) {
    decision.insert("enabled".into(), true.into());
    decision.insert("mode".into(), mode.into());
    decision.insert("model".into(), model.into());
    decision.insert("timeout_ms".into(), 5000.into());
    decision.insert("brain".into(), nested(&["url"], url.into()));
    decision.insert(
        "policy".into(),
        nested(&["minimum_model_quality"], quality.into()),
    );
}

fn nested(path: &[&str], leaf: Value) -> Value {
    path.iter().rev().fold(leaf, |inner, key| {
        let mut m = Mapping::new();
        m.insert((*key).into(), inner);
        Value::Mapping(m)
    })
}
