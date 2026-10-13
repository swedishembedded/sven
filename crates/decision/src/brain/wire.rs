// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The Brain wire: `POST /v1/run` with `{model, action, params}`.
//!
//! Everything that knows the shape of the provider's JSON lives here, so a
//! change of the wire is a change of this one file. The recorded exchanges
//! the tests pin are in `tests/fixtures/brain`.
//!
//! * `brain/decision` `models` and `select` list the models and say which one
//!   `auto` resolves to; neither loads anything.
//! * `<model id>` `decide` takes `state`, `state_format`, `questions` (a JSON
//!   document, as a string) and `deadline_ms`, and answers `outputs` with the
//!   model, the latency, the flags and one answer per question carrying the
//!   full ranked distribution.
//! * A failure is an HTTP status with an error body whose `code` names the
//!   provider's failure class.

use std::time::Duration;

use serde_json::{json, Value};

use crate::directory::{ModelInfo, ModelState, Selection};
use crate::{
    Answer, Candidate, DecisionError, DecisionRequest, DecisionResult, Distribution, ModelIdentity,
    Question, QuestionAnswer, QuestionKind,
};

/// The family model that answers "which models exist" and "which would auto
/// use".
pub(super) const DIRECTORY_MODEL: &str = "brain/decision";

/// The route, from a configured base URL with or without `/v1` or a slash.
pub(super) fn run_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/v1/run")
}

pub(super) fn call(model: &str, action: &str, params: &Value) -> Value {
    json!({"model": model, "action": action, "params": params})
}

fn wire_kind(kind: QuestionKind) -> &'static str {
    match kind {
        QuestionKind::Choice => "choice",
        QuestionKind::Score => "score",
        QuestionKind::Probability => "noul",
    }
}

fn kind_of_wire(name: &str) -> Option<QuestionKind> {
    [
        QuestionKind::Choice,
        QuestionKind::Score,
        QuestionKind::Probability,
    ]
    .into_iter()
    .find(|k| wire_kind(*k) == name)
}

fn question_json(question: &Question) -> Value {
    match question {
        Question::Choice(q) => json!({
            "id": q.id,
            "type": "choice",
            "instruction": q.instruction,
            "options": q.options.iter().map(|o| match &o.description {
                Some(d) => json!({"id": o.id, "description": d}),
                None => json!({"id": o.id}),
            }).collect::<Vec<_>>(),
        }),
        Question::Score(q) => json!({
            "id": q.id,
            "type": "score",
            "instruction": q.instruction,
            "options": q.levels,
        }),
        Question::Probability(q) => json!({
            "id": q.id,
            "type": "noul",
            "instruction": q.proposition,
        }),
    }
}

pub(super) fn decide_params(request: &DecisionRequest) -> Value {
    let questions: Vec<Value> = request.questions.iter().map(question_json).collect();
    json!({
        "state": request.state,
        "state_format": "text",
        "questions": Value::Array(questions).to_string(),
        "deadline_ms": u64::try_from(request.deadline.as_millis()).unwrap_or(u64::MAX).max(1),
    })
}

fn invalid(detail: impl Into<String>) -> DecisionError {
    DecisionError::InvalidResponse(detail.into())
}

pub(super) fn decode_models(outputs: &Value) -> Result<Vec<ModelInfo>, DecisionError> {
    let list = outputs
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("the model list has no `models` array"))?;
    list.iter().map(decode_model).collect()
}

fn decode_model(record: &Value) -> Result<ModelInfo, DecisionError> {
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("a model record has no `id`"))?;
    let state = match record.get("state").and_then(Value::as_str) {
        Some("ready") => ModelState::Ready,
        Some("not-installed") => ModelState::NotInstalled,
        Some("incompatible") => ModelState::Incompatible,
        other => return Err(invalid(format!("model {id:?} has unknown state {other:?}"))),
    };
    let manifest = record.get("manifest");
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    let question_types = manifest
        .and_then(|m| m.get("question_types"))
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str().and_then(kind_of_wire))
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                QuestionKind::Choice,
                QuestionKind::Score,
                QuestionKind::Probability,
            ]
        });
    Ok(ModelInfo {
        id: id.to_string(),
        state,
        backend: text(record.get("backend")),
        validated: record
            .get("validated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        validation_profile: text(manifest.and_then(|m| m.pointer("/validation/profile"))),
        checkpoint: text(manifest.and_then(|m| m.get("weights_sha256"))),
        max_input_tokens: manifest
            .and_then(|m| m.get("max_input_tokens"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        question_types,
        hint: text(record.get("hint")),
        detail: text(record.get("reason")),
    })
}

pub(super) fn decode_select(outputs: &Value) -> Result<Selection, DecisionError> {
    let text = |key: &str| outputs.get(key).and_then(Value::as_str).map(str::to_string);
    Ok(Selection {
        chosen: text("chosen"),
        rule: text("rule").ok_or_else(|| invalid("the selection has no `rule`"))?,
        reason: text("reason").ok_or_else(|| invalid("the selection has no `reason`"))?,
    })
}

pub(super) fn decode_decide(
    outputs: &Value,
    request: &DecisionRequest,
    resolved: &ModelInfo,
) -> Result<DecisionResult, DecisionError> {
    let model = outputs
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&resolved.id);
    let latency_ms = outputs
        .get("latency_ms")
        .and_then(Value::as_f64)
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
        .ok_or_else(|| invalid("`latency_ms` is missing or not a duration"))?;
    let flags = outputs.get("flags");
    let answers = outputs
        .get("answers")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("the response has no `answers` array"))?;
    let answers = request
        .questions
        .iter()
        .map(|q| {
            let wire = answers
                .iter()
                .find(|a| a.get("id").and_then(Value::as_str) == Some(q.id()))
                .ok_or_else(|| invalid(format!("no answer to question {:?}", q.id())))?;
            decode_answer(q, wire)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DecisionResult {
        model: ModelIdentity {
            id: model.to_string(),
            checkpoint: (model == resolved.id)
                .then(|| resolved.checkpoint.clone())
                .flatten(),
        },
        latency: Duration::from_secs_f64(latency_ms / 1000.0),
        max_input_tokens: outputs
            .get("max_input_tokens")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        state_truncated: flags
            .and_then(|f| f.get("state_truncated"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        unsupported_kinds: flags
            .and_then(|f| f.get("unsupported_question_types"))
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|n| n.as_str().and_then(kind_of_wire))
                    .collect()
            })
            .unwrap_or_default(),
        answers,
    })
}

fn decode_answer(question: &Question, wire: &Value) -> Result<QuestionAnswer, DecisionError> {
    let id = question.id();
    let kind = question.kind();
    let answer = if wire.get("unsupported").and_then(Value::as_bool) == Some(true) {
        Answer::Unsupported {
            reason: wire
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("the model does not support this question")
                .to_string(),
        }
    } else {
        if wire.get("type").and_then(Value::as_str) != Some(wire_kind(kind)) {
            return Err(invalid(format!(
                "the answer to {id:?} is not a {} answer",
                wire_kind(kind)
            )));
        }
        let ranked = wire
            .get("ranked")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("the answer to {id:?} has no `ranked` distribution")))?
            .iter()
            .map(|c| decode_candidate(id, c))
            .collect::<Result<Vec<_>, _>>()?;
        let mut distribution = Distribution::ranked(ranked)?;
        distribution.confidence = wire
            .get("confidence")
            .and_then(Value::as_f64)
            .map(|c| c as f32);
        distribution.state_truncated = wire
            .pointer("/state/truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Answer::Ranked(distribution)
    };
    Ok(QuestionAnswer {
        id: id.to_string(),
        kind,
        answer,
    })
}

fn decode_candidate(question: &str, wire: &Value) -> Result<Candidate, DecisionError> {
    let bad = || invalid(format!("a candidate of {question:?} is malformed"));
    Ok(Candidate {
        id: wire
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(bad)?
            .to_string(),
        index: wire
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(bad)?,
        probability: wire
            .get("probability")
            .and_then(Value::as_f64)
            .ok_or_else(bad)? as f32,
    })
}

/// The decision error for a failed call. The provider's own failure `code`
/// decides when it names one; otherwise the HTTP status does.
pub(super) fn classify_failure(
    status: u16,
    body: &str,
    model: &str,
    deadline: Duration,
) -> DecisionError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let code = error.and_then(|e| e.get("code")).and_then(Value::as_str);
    match code {
        Some("not_installed" | "model_not_found") => {
            return DecisionError::ModelNotInstalled {
                model: model.to_string(),
                detail: message,
            }
        }
        Some("failed_to_load") => {
            return DecisionError::FailedToLoad {
                model: model.to_string(),
                detail: message,
            }
        }
        Some("unsupported_capability") => return DecisionError::UnsupportedCapability(message),
        Some("invalid_request") => return DecisionError::InvalidRequest(message),
        Some("timeout") => return DecisionError::Timeout { deadline },
        Some("backend_failure") => {
            return DecisionError::InvalidResponse(format!("the model failed: {message}"))
        }
        _ => {}
    }
    match status {
        401 | 403 => DecisionError::Unreachable(format!(
            "the provider rejected the credentials (HTTP {status}); set decision.brain.api_key"
        )),
        429 | 503 => DecisionError::Unreachable(format!("the provider is busy (HTTP {status})")),
        504 => DecisionError::Timeout { deadline },
        404 => {
            DecisionError::UnsupportedCapability("the provider has no POST /v1/run route".into())
        }
        _ => DecisionError::InvalidResponse(format!(
            "unexpected HTTP {status}: {}",
            body.chars().take(120).collect::<String>()
        )),
    }
}
