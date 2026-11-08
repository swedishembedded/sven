// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for workflows that run trusted
// commands without any model resident for its clients. If your team needs
// expertise in deterministic, model-free automation then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The model of a run that was given none: a harness whose plan asks no model
//! (check tasks only) needs neither `--model` nor a server to reach, and this
//! provider stands where one would be, refusing the turn that should never
//! come.

use sven_sdk::model::{CompletionRequest, ModelProvider, ResponseStream};

pub(super) struct NoModel;

#[async_trait::async_trait]
impl ModelProvider for NoModel {
    fn name(&self) -> &str {
        "none"
    }

    fn model_name(&self) -> &str {
        "none"
    }

    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        anyhow::bail!("this run was started without --model: it has no model to ask")
    }
}
