// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that keep every answer a model
// gives exactly as it was sampled, for its clients. If your team needs
// expertise in agent histories that can be trained on as they ran then you can
// procure our services by sending an email to info@swedishembedded.com.
//! A model's reply as it arrived: [`SampledReply`].
//!
//! A reply is never changed after it streamed in: its text, its reasoning and
//! each call's argument text are kept byte for byte, and a server's `sampled`
//! blob (the ids it generated, which it splices back into the next prompt) is
//! kept as the exact JSON it sent, never decoded. The history sent with every
//! later request is this reply, so each request extends the previous one and
//! what the model generated. A call's arguments are parsed only to run the
//! tool ([`SampledCall::arguments`]); a call whose text is not a JSON object
//! is not run, and its text stays in the history as written.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

/// How a reply ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyFinish {
    /// The model ended it.
    Stop,
    /// The output-token limit cut it; a call still open was never completed.
    Length,
}

/// One tool call of a reply, as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampledCall {
    pub id: String,
    pub name: String,
    /// The argument text as streamed, never re-serialised.
    pub arguments: String,
}

/// Why a call's argument text cannot run its tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// The text is not JSON.
    NotJson(String),
    /// The text is JSON, but not an object.
    NotObject,
    /// The reply was cut before the call's arguments ended.
    CutOff,
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::NotJson(e) => write!(f, "the arguments are not valid JSON ({e})"),
            ArgError::NotObject => write!(f, "the arguments are JSON but not an object"),
            ArgError::CutOff => write!(
                f,
                "the reply reached its output limit before the arguments ended"
            ),
        }
    }
}

impl std::error::Error for ArgError {}

impl SampledCall {
    /// The arguments to run the tool with. An empty text is a call with no
    /// arguments (`{}`), which some servers send for a tool without
    /// parameters; anything else must be a JSON object.
    pub fn arguments(&self) -> Result<Value, ArgError> {
        if self.arguments.trim().is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        match serde_json::from_str::<Value>(&self.arguments) {
            Ok(v @ Value::Object(_)) => Ok(v),
            Ok(_) => Err(ArgError::NotObject),
            Err(e) => Err(ArgError::NotJson(e.to_string())),
        }
    }
}

/// A model's reply, kept exactly as it arrived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampledReply {
    /// The text, as streamed.
    pub text: String,
    /// The reasoning, as streamed (every reasoning delta, in order); empty
    /// when the model did not reason.
    pub reasoning: String,
    /// The calls, in the order the model wrote them.
    pub calls: Vec<SampledCall>,
    /// The server's record of what it sampled for this reply, as the exact
    /// JSON it sent; echoed unchanged with the reply in every later request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled: Option<Box<RawValue>>,
    pub finish: ReplyFinish,
}

impl SampledReply {
    /// A reply that holds nothing: no text, no call (reasoning aside).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.calls.is_empty()
    }

    /// Whether the call `index` was cut by the output limit: the last call of
    /// a reply that ended at its limit, with arguments that do not parse. One
    /// whose arguments closed was written whole.
    #[must_use]
    pub fn cut_off(&self, index: usize) -> bool {
        self.finish == ReplyFinish::Length
            && index + 1 == self.calls.len()
            && self.calls[index].arguments().is_err()
    }

    /// The arguments call `index` runs with: [`SampledCall::arguments`], or
    /// [`ArgError::CutOff`] for a call the output limit cut.
    pub fn arguments(&self, index: usize) -> Result<Value, ArgError> {
        if self.cut_off(index) {
            return Err(ArgError::CutOff);
        }
        self.calls[index].arguments()
    }
}

/// Two replies are the same when every part is, the `sampled` record compared
/// as the text it is.
impl PartialEq for SampledReply {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.reasoning == other.reasoning
            && self.calls == other.calls
            && self.sampled.as_deref().map(RawValue::get)
                == other.sampled.as_deref().map(RawValue::get)
            && self.finish == other.finish
    }
}
