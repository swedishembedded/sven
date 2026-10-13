// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The Brain transport adapter: a [`DecisionProvider`] that reaches a Brain
//! server over HTTP (`POST /v1/run`).
//!
//! * Every failure becomes a [`DecisionError`] class; the deadline bounds the
//!   whole decision, resolution and queueing included.
//! * At most [`BrainSettings::max_concurrent_requests`] decisions are in
//!   flight; the others wait their turn inside their own deadline.
//! * `auto` is resolved by the provider, not here: the adapter asks which
//!   model `auto` means (a query that loads nothing), remembers the answer for
//!   the life of the provider, and reports the model it got in every result.
//! * Decisions of a model that declares its checkpoint identity are cached
//!   ([`crate::DecisionCache`]); batching of independent questions is
//!   [`crate::decide_batched`], which works over any provider.

mod wire;

use std::num::NonZeroUsize;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore};

use crate::directory::resolve_model;
pub use crate::directory::{MinimumQuality, ModelInfo, ModelState, Selection, AUTO};
use crate::{DecisionCache, DecisionError, DecisionProvider, DecisionRequest, DecisionResult};

/// The address `brain serve --openai` listens on unless told otherwise.
pub const DEFAULT_URL: &str = "http://127.0.0.1:8788";

/// How a Brain provider is reached and held to account.
#[derive(Debug, Clone)]
pub struct BrainSettings {
    /// The server's base URL, with or without `/v1`.
    pub base_url: String,
    /// Sent as a bearer token when set.
    pub api_key: Option<String>,
    /// [`AUTO`] or a stable model id.
    pub model: String,
    pub minimum_quality: MinimumQuality,
    /// Most decisions in flight at once; at least 1.
    pub max_concurrent_requests: usize,
    /// Most decisions the cache holds; at least 1.
    pub cache_entries: usize,
}

impl BrainSettings {
    /// Settings for the server at `base_url`: model `auto`, any quality, two
    /// decisions in flight, 256 cached decisions.
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            api_key: None,
            model: AUTO.to_string(),
            minimum_quality: MinimumQuality::Any,
            max_concurrent_requests: 2,
            cache_entries: 256,
        }
    }
}

/// A decision provider backed by a Brain server.
pub struct BrainProvider {
    settings: BrainSettings,
    run_url: String,
    http: reqwest::Client,
    permits: Semaphore,
    resolved: Mutex<Option<ModelInfo>>,
    cache: DecisionCache,
}

impl BrainProvider {
    /// A provider for `settings`. Contacts nothing.
    ///
    /// # Errors
    ///
    /// A message when a bound in `settings` is zero or the HTTP client cannot
    /// be built.
    pub fn new(settings: BrainSettings) -> Result<Self, String> {
        if settings.max_concurrent_requests == 0 {
            return Err("max_concurrent_requests must be at least 1".into());
        }
        let cache_entries =
            NonZeroUsize::new(settings.cache_entries).ok_or("cache_entries must be at least 1")?;
        // No proxy: where decisions go is the configured URL, never the
        // process environment.
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
        Ok(Self {
            run_url: wire::run_url(&settings.base_url),
            permits: Semaphore::new(settings.max_concurrent_requests),
            cache: DecisionCache::new(cache_entries),
            resolved: Mutex::new(None),
            http,
            settings,
        })
    }

    /// The settings the provider was built from.
    #[must_use]
    pub fn settings(&self) -> &BrainSettings {
        &self.settings
    }

    /// Every decision model the server knows, with its state, validation and
    /// declared limits. Loads nothing.
    ///
    /// # Errors
    ///
    /// The class of the failure.
    pub async fn models(&self, timeout: Duration) -> Result<Vec<ModelInfo>, DecisionError> {
        let outputs = self
            .run(
                wire::DIRECTORY_MODEL,
                "models",
                &serde_json::json!({}),
                timeout,
            )
            .await?;
        wire::decode_models(&outputs)
    }

    /// The model the server would use for `requested` (`auto` or an id).
    /// Loads nothing.
    ///
    /// # Errors
    ///
    /// The class of the failure.
    pub async fn select(
        &self,
        requested: &str,
        timeout: Duration,
    ) -> Result<Selection, DecisionError> {
        let outputs = self
            .run(
                wire::DIRECTORY_MODEL,
                "select",
                &serde_json::json!({"model": requested}),
                timeout,
            )
            .await?;
        wire::decode_select(&outputs)
    }

    /// The configured model, resolved and checked against the minimum
    /// quality, without loading it.
    ///
    /// # Errors
    ///
    /// Why no model can be used.
    pub async fn readiness(&self, timeout: Duration) -> Result<ModelInfo, DecisionError> {
        let selection = if self.settings.model == AUTO {
            Some(self.select(AUTO, timeout).await?)
        } else {
            None
        };
        let models = self.models(timeout).await?;
        resolve_model(
            &self.settings.model,
            &models,
            selection.as_ref(),
            self.settings.minimum_quality,
        )
    }

    /// The model decisions go to. Resolved once for concurrent callers: the
    /// lock is held across the lookup, so a burst of first decisions asks the
    /// provider once.
    async fn resolved_model(&self, timeout: Duration) -> Result<ModelInfo, DecisionError> {
        let mut resolved = self.resolved.lock().await;
        if let Some(model) = resolved.as_ref() {
            return Ok(model.clone());
        }
        let model = self.readiness(timeout).await?;
        *resolved = Some(model.clone());
        Ok(model)
    }

    /// One `POST /v1/run`, returning its `outputs`.
    async fn run(
        &self,
        model: &str,
        action: &str,
        params: &Value,
        deadline: Duration,
    ) -> Result<Value, DecisionError> {
        let exchange = async {
            let mut call = self
                .http
                .post(&self.run_url)
                .json(&wire::call(model, action, params));
            if let Some(key) = &self.settings.api_key {
                call = call.bearer_auth(key);
            }
            let response = call
                .send()
                .await
                .map_err(|e| DecisionError::Unreachable(describe(e, &self.run_url)))?;
            let status = response.status().as_u16();
            let body = response
                .text()
                .await
                .map_err(|e| DecisionError::Unreachable(describe(e, &self.run_url)))?;
            if status != 200 {
                return Err(wire::classify_failure(status, &body, model, deadline));
            }
            let mut document: Value = serde_json::from_str(&body).map_err(|e| {
                DecisionError::InvalidResponse(format!("the body is not JSON: {e}"))
            })?;
            document.get_mut("outputs").map(Value::take).ok_or_else(|| {
                DecisionError::InvalidResponse("the response has no `outputs`".into())
            })
        };
        tokio::time::timeout(deadline, exchange)
            .await
            .unwrap_or(Err(DecisionError::Timeout { deadline }))
    }

    async fn decide_within(
        &self,
        request: &DecisionRequest,
    ) -> Result<DecisionResult, DecisionError> {
        let model = self.resolved_model(request.deadline).await?;
        let key = DecisionCache::key(
            &crate::ModelIdentity {
                id: model.id.clone(),
                checkpoint: model.checkpoint.clone(),
            },
            request,
        );
        if let Some(hit) = key.as_ref().and_then(|k| self.cache.get(k)) {
            return Ok(DecisionResult { cached: true, ..hit });
        }
        let _slot = self
            .permits
            .acquire()
            .await
            .map_err(|_| DecisionError::Unreachable("the provider was shut down".into()))?;
        let outputs = match self
            .run(
                &model.id,
                "decide",
                &wire::decide_params(request),
                request.deadline,
            )
            .await
        {
            Ok(outputs) => outputs,
            Err(error) => {
                self.forget_model_if_gone(&error).await;
                return Err(error);
            }
        };
        let result = wire::decode_decide(&outputs, request, &model)?;
        if let Some(key) = key {
            self.cache.put(key, result.clone());
        }
        Ok(result)
    }

    /// A model that has gone away is looked up again on the next decision.
    async fn forget_model_if_gone(&self, error: &DecisionError) {
        if matches!(
            error,
            DecisionError::ModelNotInstalled { .. } | DecisionError::FailedToLoad { .. }
        ) {
            *self.resolved.lock().await = None;
        }
    }
}

fn describe(error: reqwest::Error, url: &str) -> String {
    let kind = if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    format!("{kind} for {url}: {}", error.without_url())
}

#[async_trait]
impl DecisionProvider for BrainProvider {
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        request.validate()?;
        tokio::time::timeout(request.deadline, self.decide_within(request))
            .await
            .unwrap_or(Err(DecisionError::Timeout {
                deadline: request.deadline,
            }))
    }
}
