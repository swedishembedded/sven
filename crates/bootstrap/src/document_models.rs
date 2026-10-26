// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Which OCR model `document_convert` and `sven document convert` use.
//!
//! The model comes from, in order: the explicit request, `ocr.model`, and
//! DeepSeek-OCR-2 (or else any OCR model) discovered on the reachable
//! providers. A loose name ("deepseek ocr v2") is matched to the configured
//! `ocr.models` aliases and to what the providers report they serve. Nothing
//! falls back to a chat model: when no OCR model can be had the caller gets
//! [`NoOcrModel::Unavailable`] and must tell the user.
//!
//! Swedish Embedded AB implements model routing for document pipelines for its
//! clients. If your team needs expertise in running OCR models across several
//! servers then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use async_trait::async_trait;
use sven_document::{match_model, Candidate, MatchError};
use sven_model::ModelProvider;
use sven_tools_document::{NoOcrModel, OcrModel, OcrModels};

use crate::config::OcrConfig;
use crate::Config;

/// What is tried when neither the request nor `ocr.model` names a model.
const DEFAULT_QUERY: &str = "deepseek ocr 2";

/// Conversion options with the `ocr` section's values over the defaults.
pub fn convert_options(ocr: &OcrConfig) -> sven_document::ConvertOptions {
    let mut o = sven_document::ConvertOptions::default();
    if let Some(v) = ocr.concurrency {
        o.concurrency = v;
    }
    if let Some(v) = ocr.dpi {
        o.dpi = v;
    }
    if let Some(v) = ocr.max_tokens {
        o.max_tokens = v;
    }
    if let Some(v) = ocr.layout.as_deref().and_then(|v| v.parse().ok()) {
        o.layout = v;
    }
    if let Some(v) = &ocr.prompt {
        o.prompt = v.clone();
    }
    o
}

pub struct ConfigOcrModels {
    config: Config,
}

impl ConfigOcrModels {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Configured aliases first, then every OCR model a reachable provider
    /// reports (an id containing "ocr").
    async fn candidates(&self) -> (Vec<Candidate>, Vec<String>) {
        let mut out: Vec<Candidate> = self
            .config
            .ocr
            .models
            .iter()
            .filter_map(|(alias, specs)| {
                Some(Candidate {
                    spec: specs.as_slice().first()?.to_string(),
                    alias: Some(alias.clone()),
                })
            })
            .collect();
        let found = sven_model_drivers::reachable::reachable_models(&self.config.providers).await;
        for m in found.models {
            if m.id.to_lowercase().contains("ocr") && !out.iter().any(|c| c.spec == m.reference) {
                out.push(Candidate {
                    spec: m.reference,
                    alias: None,
                });
            }
        }
        let problems = found
            .problems
            .into_iter()
            .map(|p| format!("{}: {}", p.source, p.problem))
            .collect();
        (out, problems)
    }

    async fn named(&self, wanted: &str) -> Result<OcrModel, NoOcrModel> {
        if let Some(specs) = self.config.ocr.models.get(wanted) {
            return self.build(wanted, &specs.as_slice()).await;
        }
        let (candidates, problems) = self.candidates().await;
        match match_model(wanted, &candidates) {
            Ok(found) => {
                let specs = self
                    .config
                    .ocr
                    .models
                    .get(found.alias.as_deref().unwrap_or_default());
                match specs {
                    Some(s) => self.build(&found.spec, &s.as_slice()).await,
                    None => self.build(&found.spec, &[found.spec.as_str()]).await,
                }
            }
            // A full spec the providers do not list (an OpenRouter model, say).
            Err(MatchError::NoMatch { .. }) if wanted.contains('/') => {
                self.build(wanted, &[wanted]).await
            }
            Err(MatchError::NoMatch { .. }) if candidates.is_empty() => {
                Err(NoOcrModel::Unavailable {
                    reason: unavailable_reason(&problems),
                })
            }
            Err(e) => Err(NoOcrModel::Refused(e.to_string())),
        }
    }

    async fn discovered(&self) -> Result<OcrModel, NoOcrModel> {
        let (candidates, problems) = self.candidates().await;
        if candidates.is_empty() {
            return Err(NoOcrModel::Unavailable {
                reason: unavailable_reason(&problems),
            });
        }
        let pick = match_model(DEFAULT_QUERY, &candidates)
            .cloned()
            .or_else(|_| {
                // No OCR-2: any one OCR model is what the user has on offer.
                candidates.first().cloned().ok_or(MatchError::NoMatch {
                    query: String::new(),
                    choices: vec![],
                })
            })
            .map_err(|e| NoOcrModel::Refused(e.to_string()))?;
        self.build(&pick.spec, &[pick.spec.as_str()]).await
    }

    /// Providers for `specs` (replicas of one model), checked to take images
    /// and, when local, to be answering.
    async fn build(&self, identity: &str, specs: &[&str]) -> Result<OcrModel, NoOcrModel> {
        let mut providers: Vec<Arc<dyn ModelProvider>> = Vec::new();
        for spec in specs {
            let cfg = self.config.resolve_model(spec);
            let provider: Arc<dyn ModelProvider> =
                Arc::from(sven_model_drivers::from_config(&cfg).map_err(|e| {
                    NoOcrModel::Unavailable {
                        reason: format!("model '{spec}' cannot be set up: {e}"),
                    }
                })?);
            if !provider.supports_images() {
                return Err(NoOcrModel::Unavailable {
                    reason: format!(
                        "model '{spec}' does not accept images; declare `input_modalities: [text, image]` on its provider entry or pick an OCR model"
                    ),
                });
            }
            if provider.is_local() && provider.discover().await.is_none() {
                return Err(NoOcrModel::Unavailable {
                    reason: format!("the server for '{spec}' is not answering"),
                });
            }
            providers.push(provider);
        }
        Ok(OcrModel {
            spec: identity.to_string(),
            providers,
        })
    }
}

fn unavailable_reason(problems: &[String]) -> String {
    if problems.is_empty() {
        "none configured in `ocr.model` and none served by any configured provider".into()
    } else {
        format!(
            "none configured in `ocr.model`, none served by a reachable provider ({})",
            problems.join("; ")
        )
    }
}

#[async_trait]
impl OcrModels for ConfigOcrModels {
    async fn resolve(&self, requested: Option<&str>) -> Result<OcrModel, NoOcrModel> {
        match requested.or(self.config.ocr.model.as_deref()) {
            Some(wanted) => self.named(wanted).await,
            None => self.discovered().await,
        }
    }
}
