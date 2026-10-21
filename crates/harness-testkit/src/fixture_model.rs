// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Fixture models: the only model sources a case may name.
//!
//! `sequence` reuses the scripted mock (one item per model call, in order),
//! `rules` the YAML mock (stateless and prompt-matched, for runs whose
//! turns do not arrive in a fixed order), `fail` the failing mock. `live` is
//! parsed so a manifest can name it, but it is built only under an explicit
//! [`OptIn`].
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use sven_model_mock::{FailingMockProvider, ScriptedMockProvider, YamlMockProvider};
use sven_sdk::model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};

use crate::dir::HarnessDir;
use crate::error::TestkitError;

/// A model source named by a case.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawModel")]
pub enum ModelFixture {
    /// One scripted answer per model call, in order.
    Sequence(Vec<SequenceItem>),
    /// Stateless prompt-matched rules (the YAML mock's `responses`).
    Rules(Vec<serde_yaml::Value>),
    /// A file (inside the harness directory) holding another model block.
    File(String),
    /// Every call fails with this message.
    Fail(String),
    /// One fixture per run seed: a deterministic stand-in for a stochastic
    /// model, so a paired comparison can give both variants the same
    /// per-seed behaviour. Running without a seed, or with a seed the block
    /// does not name, is an error.
    BySeed(BTreeMap<u64, ModelFixture>),
    /// A real local model, e.g. `ollama/qwen3:8b`. Opt-in only.
    Live {
        /// `provider/name` of a model the caller is already serving.
        model: String,
    },
}

/// One scripted model answer.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceItem {
    /// The answer text.
    #[serde(default)]
    pub text: Option<String>,
    /// Tool calls the model proposes.
    #[serde(default)]
    pub tool_calls: Vec<SequenceToolCall>,
    /// Token usage the provider reports for the call.
    #[serde(default)]
    pub usage: Option<SequenceUsage>,
}

/// A tool call a scripted answer proposes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceToolCall {
    /// The call id.
    pub id: String,
    /// The tool's name.
    pub name: String,
    /// The call arguments.
    #[serde(default = "empty_args")]
    pub args: Value,
}

fn empty_args() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Reported token usage for one scripted answer.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceUsage {
    /// Prompt tokens.
    #[serde(default)]
    pub input: u32,
    /// Generated tokens.
    #[serde(default)]
    pub output: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModel {
    #[serde(default)]
    sequence: Option<Vec<SequenceItem>>,
    #[serde(default)]
    rules: Option<Vec<serde_yaml::Value>>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    fail: Option<String>,
    #[serde(default)]
    live: Option<RawLive>,
    #[serde(default)]
    by_seed: Option<BTreeMap<u64, ModelFixture>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLive {
    model: String,
}

impl TryFrom<RawModel> for ModelFixture {
    type Error = String;

    fn try_from(raw: RawModel) -> Result<Self, String> {
        let named = [
            raw.sequence.is_some(),
            raw.rules.is_some(),
            raw.file.is_some(),
            raw.fail.is_some(),
            raw.live.is_some(),
            raw.by_seed.is_some(),
        ];
        if named.iter().filter(|set| **set).count() != 1 {
            return Err(
                "a model block names exactly one of `sequence`, `rules`, `file`, `fail`, `by_seed` or `live`"
                    .to_string(),
            );
        }
        Ok(if let Some(items) = raw.sequence {
            Self::Sequence(items)
        } else if let Some(rules) = raw.rules {
            Self::Rules(rules)
        } else if let Some(path) = raw.file {
            Self::File(path)
        } else if let Some(message) = raw.fail {
            Self::Fail(message)
        } else if let Some(by_seed) = raw.by_seed {
            Self::BySeed(by_seed)
        } else {
            Self::Live {
                model: raw.live.map(|l| l.model).unwrap_or_default(),
            }
        })
    }
}

/// The explicit permission to use a real, already-running model.
///
/// It is built only by [`OptIn::live`], from values the caller names on a
/// command line: nothing here reads the environment, and the default is no
/// opt-in. The testkit never starts, pulls or discovers a model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptIn {
    /// Overrides the model a live case names (`provider/name`).
    model: Option<String>,
    /// The base URL of the running server.
    endpoint: Option<String>,
}

impl OptIn {
    /// No opt-in: mock models only.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Opts in to the server at `endpoint`, optionally replacing the model
    /// spec a live case names. A blank endpoint is no endpoint, so the
    /// result is not live-ready.
    #[must_use]
    pub fn live(model: Option<&str>, endpoint: &str) -> Self {
        let endpoint = endpoint.trim();
        Self {
            model: model
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string),
            endpoint: (!endpoint.is_empty()).then(|| endpoint.to_string()),
        }
    }

    /// Whether a live model may be built.
    #[must_use]
    pub fn is_live_ready(&self) -> bool {
        self.endpoint.is_some()
    }
}

/// A provider and the books kept on it for the case's verdict.
pub(crate) struct BuiltModel {
    pub provider: Arc<dyn ModelProvider>,
    /// Calls made against a finite sequence, when it is one.
    pub usage: Arc<ModelUse>,
}

/// How many calls a sequence served against how many it holds.
pub(crate) struct ModelUse {
    pub calls: AtomicUsize,
    /// `None` for sources that answer any number of calls.
    pub budget: Option<usize>,
}

impl ModelUse {
    /// A message when a finite sequence was asked for more than it holds.
    pub(crate) fn exhausted(&self) -> Option<String> {
        let calls = self.calls.load(Ordering::SeqCst);
        match self.budget {
            Some(budget) if calls > budget => Some(format!(
                "model sequence exhausted: the harness made {calls} model calls, the fixture holds {budget}"
            )),
            _ => None,
        }
    }
}

/// Counts calls on its way to the wrapped provider.
struct Counted {
    inner: Arc<dyn ModelProvider>,
    usage: Arc<ModelUse>,
}

#[async_trait::async_trait]
impl ModelProvider for Counted {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        self.usage.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.complete(req).await
    }
}

impl ModelFixture {
    /// Whether this fixture may be built under `opt_in`; the reason when
    /// not. Every non-live fixture always may.
    ///
    /// # Errors
    ///
    /// A message naming the flag that is missing.
    pub fn live_gate(&self, opt_in: &OptIn) -> Result<(), String> {
        match self {
            Self::Live { .. } => {}
            Self::BySeed(by_seed) => {
                return by_seed.values().try_for_each(|f| f.live_gate(opt_in));
            }
            _ => return Ok(()),
        }
        if !opt_in.is_live_ready() {
            return Err(
                "a live model needs an explicit endpoint: pass --model-endpoint <url> of a server you are already running"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// Builds the provider a run will use. `None` is a model that only fails.
pub(crate) fn build(
    fixture: Option<&ModelFixture>,
    dir: &HarnessDir,
    opt_in: &OptIn,
    seed: Option<u64>,
) -> Result<BuiltModel, TestkitError> {
    let unbounded = |provider: Arc<dyn ModelProvider>| BuiltModel {
        provider,
        usage: Arc::new(ModelUse {
            calls: AtomicUsize::new(0),
            budget: None,
        }),
    };
    let Some(fixture) = fixture else {
        return Ok(unbounded(Arc::new(FailingMockProvider::new(
            "no model fixture",
        ))));
    };
    fixture
        .live_gate(opt_in)
        .map_err(TestkitError::LiveNotOptedIn)?;
    match fixture {
        ModelFixture::Sequence(items) => {
            let usage = Arc::new(ModelUse {
                calls: AtomicUsize::new(0),
                budget: Some(items.len()),
            });
            let scripted = ScriptedMockProvider::new(items.iter().map(script_of).collect());
            let provider = Arc::new(Counted {
                inner: Arc::new(scripted),
                usage: Arc::clone(&usage),
            });
            Ok(BuiltModel { provider, usage })
        }
        ModelFixture::Rules(rules) => {
            let text = serde_yaml::to_string(&serde_json::json!({ "responses": rules }))
                .map_err(|e| TestkitError::Manifest(format!("unusable model rules: {e}")))?;
            let mock = YamlMockProvider::load(&text)
                .map_err(|e| TestkitError::Manifest(format!("unusable model rules: {e:#}")))?;
            Ok(unbounded(Arc::new(mock)))
        }
        ModelFixture::File(rel) => {
            let path = dir.resolve(rel)?;
            let text = std::fs::read_to_string(&path).map_err(|source| TestkitError::Io {
                path: path.clone(),
                source,
            })?;
            let inner: ModelFixture = serde_yaml::from_str(&text).map_err(|e| {
                TestkitError::Manifest(format!("{}: invalid model fixture: {e}", path.display()))
            })?;
            if matches!(inner, ModelFixture::File(_)) {
                return Err(TestkitError::Manifest(format!(
                    "{}: a model file cannot name another model file",
                    path.display()
                )));
            }
            build(Some(&inner), dir, opt_in, seed)
        }
        ModelFixture::BySeed(by_seed) => {
            let seed = seed.ok_or_else(|| {
                TestkitError::Manifest(
                    "the model fixture is `by_seed` but the run has no seed: pass --seed".into(),
                )
            })?;
            let inner = by_seed.get(&seed).ok_or_else(|| {
                TestkitError::Manifest(format!(
                    "the model fixture names no behaviour for seed {seed} (has: {})",
                    by_seed
                        .keys()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            if matches!(inner, ModelFixture::BySeed(_)) {
                return Err(TestkitError::Manifest(
                    "a `by_seed` block cannot nest another `by_seed`".into(),
                ));
            }
            build(Some(inner), dir, opt_in, Some(seed))
        }
        ModelFixture::Fail(message) => Ok(unbounded(Arc::new(FailingMockProvider::new(
            message.clone(),
        )))),
        ModelFixture::Live { model } => {
            let endpoint = opt_in.endpoint.clone().unwrap_or_default();
            let model = opt_in.model.as_ref().unwrap_or(model);
            let (provider, name) = model.split_once('/').unwrap_or(("ollama", model.as_str()));
            let mut config = sven_sdk::config::Config::default();
            config.model.provider = provider.to_string();
            config.model.name = name.to_string();
            config.model.base_url = Some(endpoint);
            let provider = sven_sdk::drivers::from_config(&config.model)
                .map_err(|e| TestkitError::Run(format!("cannot build the live model: {e:#}")))?;
            Ok(unbounded(Arc::from(provider)))
        }
    }
}

/// The response events a scripted item plays.
fn script_of(item: &SequenceItem) -> Vec<ResponseEvent> {
    let mut events = Vec::new();
    if let Some(text) = &item.text {
        events.push(ResponseEvent::TextDelta(text.clone()));
    }
    for (index, call) in item.tool_calls.iter().enumerate() {
        events.push(ResponseEvent::ToolCall {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.args.to_string(),
        });
    }
    if let Some(usage) = item.usage {
        events.push(ResponseEvent::Usage {
            input_tokens: usage.input,
            output_tokens: usage.output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
        });
    }
    events.push(ResponseEvent::Done);
    events
}
