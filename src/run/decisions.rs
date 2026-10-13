// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `sven doctor decisions` and `sven decisions models`.

use std::time::Duration;

use anyhow::Context;
use sven_bootstrap::Config;
use sven_decision::brain::BrainProvider;
use sven_decision::doctor::{render_models, run_doctor};

/// Prints the decision doctor's report; an error (non-zero exit) when
/// decisions are unavailable.
pub(crate) async fn run_doctor_decisions(config: &Config, json: bool) -> anyhow::Result<()> {
    let report = run_doctor(&config.decision).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render());
    }
    anyhow::ensure!(report.is_ready(), "decisions are unavailable");
    Ok(())
}

/// Prints the models the provider knows.
pub(crate) async fn run_decisions_models(config: &Config, json: bool) -> anyhow::Result<()> {
    config.decision.validate().map_err(anyhow::Error::msg)?;
    let provider =
        BrainProvider::new(config.decision.brain_settings()).map_err(anyhow::Error::msg)?;
    let models = provider
        .models(Duration::from_secs(5))
        .await
        .map_err(|e| anyhow::anyhow!(sven_decision::unavailable_text(&config.decision, &e)))
        .context("listing decision models")?;
    if json {
        let list: Vec<_> = models
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "state": m.state.name(),
                    "backend": m.backend,
                    "validated": m.validated,
                    "validation_profile": m.validation_profile,
                    "question_types": m.question_types.iter().map(|k| k.name()).collect::<Vec<_>>(),
                    "max_input_tokens": m.max_input_tokens,
                    "hint": m.hint,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
    } else {
        print!("{}", render_models(&config.decision, &models));
    }
    Ok(())
}
