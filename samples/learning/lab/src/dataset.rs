// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! L7 - turning verified episodes into training data.
//!
//! The output is `generic-messages-v2`: one packed conversation per line,
//! `train` per message deciding which spans are supervised. brain's parser is
//! strict about it, and this module is strict about the things the parser
//! cannot see.
//!
//! # What this refuses to emit, and why
//!
//! **An episode that was not verified.** Training on an unverified trajectory
//! teaches whatever the model happened to do, which on a task it fails is
//! precisely the behaviour being trained out of it. Only a [`Verdict`] that
//! says solved gets through, and a [`Verdict`] can only come from evaluating
//! a task's declared predicates.
//!
//! **A transcript with a hole in it.** A [`Turn::Opaque`] is a message this
//! harness could not represent. Emitting the conversation without it would
//! produce an example whose context is missing a step the model actually saw,
//! and nothing downstream could detect that. The episode is dropped with a
//! reason instead.
//!
//! **Tool output as a supervised span.** Only assistant turns are trained on.
//! Supervising a tool result teaches the model to produce tool output itself -
//! to hallucinate the answer it was supposed to go and fetch, which is again
//! the exact failure the task exists to correct.
//!
//! # Provenance is not decoration
//!
//! Every record carries how it was produced. A trajectory a stronger model
//! generated, or one a script drove, is excellent supervised data and is *not*
//! an on-policy rollout; mixing them and forgetting which was which is how a
//! later claim about self-improvement quietly stops being true. The metadata
//! travels with the record so the question stays answerable afterwards.

use serde::{Deserialize, Serialize};
use sven_sdk::Turn;

use crate::Verdict;

/// How a trajectory came to exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// The model being trained produced it.
    OnPolicy,
    /// A different, usually stronger, model produced it.
    Teacher,
    /// A deterministic solver drove it. Verified and useful, but it
    /// demonstrates one path rather than a policy.
    Scripted,
}

/// Why an episode produced no training record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Excluded {
    /// The verifier did not accept the work.
    NotSolved { unmet: Vec<String> },
    /// The transcript contains a message this harness could not represent, so
    /// any example drawn from it would be missing context the model saw.
    OpaqueTurn { role: String },
    /// Nothing in the episode would be supervised.
    NothingToLearn,
}

impl std::fmt::Display for Excluded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Excluded::NotSolved { unmet } => {
                write!(
                    f,
                    "the verifier did not accept it; unmet: {}",
                    unmet.join(", ")
                )
            }
            Excluded::OpaqueTurn { role } => write!(
                f,
                "the transcript contains a {role} message this harness cannot represent, so an \
                 example drawn from it would be missing a step the model actually saw"
            ),
            Excluded::NothingToLearn => f.write_str(
                "no assistant turn would be supervised, so the record would train on nothing",
            ),
        }
    }
}

/// One `generic-messages-v2` message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Required on every message by the consuming parser: a record with no
    /// explicit supervision boundary is either a silent no-op or a silent
    /// prompt leak into the loss.
    pub train: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: WireFunction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFunction {
    pub name: String,
    /// JSON-encoded argument text, exactly as the model emitted it.
    pub arguments: String,
}

/// One packed conversation, ready to serialise as a JSONL line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub messages: Vec<WireMessage>,
    #[serde(default)]
    pub tools: Vec<serde_json::Value>,
    pub metadata: RecordMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMetadata {
    /// The task family. Splits are keyed on this, never on the episode: two
    /// episodes of one family share a workspace, a request and a solution
    /// shape, so splitting by episode leaks near-duplicate context across
    /// train and test and reports memorisation as generalisation.
    pub family: String,
    pub provenance: Provenance,
    /// What was hidden for this episode. Recorded after the fact, for
    /// auditing the mix - never shown to a model.
    pub hidden_state: String,
}

/// A message the model was shown but is not trained on.
///
/// Everything that is not an assistant decision is context: the system
/// prompt, the request, and every tool result. Supervising any of them would
/// teach the model to produce them.
fn context(role: &str, text: &str) -> WireMessage {
    WireMessage {
        role: role.to_string(),
        content: text.to_string(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        train: false,
    }
}

/// Derive a training record from one verified episode.
pub fn record_from_episode(
    family: &str,
    hidden_state: &str,
    provenance: Provenance,
    transcript: &[Turn],
    verdict: &Verdict,
) -> Result<Record, Excluded> {
    if !verdict.solved() {
        return Err(Excluded::NotSolved {
            unmet: verdict.failed().to_vec(),
        });
    }

    let mut messages: Vec<WireMessage> = Vec::new();
    for turn in transcript {
        match turn {
            Turn::Opaque { role } => return Err(Excluded::OpaqueTurn { role: role.clone() }),
            Turn::System { text } => messages.push(context("system", text)),
            Turn::User { text } => messages.push(context("user", text)),
            Turn::ToolResult { call_id, text } => {
                let mut message = context("tool", text);
                message.tool_call_id = Some(call_id.clone());
                messages.push(message);
            }
            Turn::Assistant { text, tool_calls } => {
                let calls: Vec<WireToolCall> = tool_calls
                    .iter()
                    .map(|c| WireToolCall {
                        id: Some(c.id.clone()),
                        kind: "function".into(),
                        function: WireFunction {
                            name: c.name.clone(),
                            arguments: c.arguments.clone(),
                        },
                    })
                    .collect();

                // The kernel stores one model turn's several calls as several
                // messages. Merge them back into one assistant message: that
                // is the shape the model produced and the shape it must learn
                // to produce, and training on a split version would teach it
                // to stop after the first call.
                match messages.last_mut() {
                    Some(last)
                        if last.role == "assistant"
                            && last.content.is_empty()
                            && !calls.is_empty() =>
                    {
                        last.tool_calls.extend(calls);
                        if !text.is_empty() {
                            last.content = text.clone();
                        }
                    }
                    _ => messages.push(WireMessage {
                        role: "assistant".into(),
                        content: text.clone(),
                        tool_calls: calls,
                        tool_call_id: None,
                        // The supervised span, and the only one.
                        train: true,
                    }),
                }
            }
        }
    }

    if !messages.iter().any(|m| m.train) {
        return Err(Excluded::NothingToLearn);
    }

    Ok(Record {
        messages,
        tools: Vec::new(),
        metadata: RecordMetadata {
            family: family.to_string(),
            provenance,
            hidden_state: hidden_state.to_string(),
        },
    })
}

/// Serialise records as JSONL, one packed conversation per line.
pub fn to_jsonl(records: &[Record]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for record in records {
        out.push_str(&serde_json::to_string(record)?);
        out.push('\n');
    }
    Ok(out)
}

/// Derive a training record from the requests an agent actually sent.
///
/// The last request's `messages` array is the whole conversation as the server
/// was given it: the real system prompt, every real tool result, and the
/// assistant turns in between. Using it rather than the agent's stored history
/// is what makes the record render at training time the way the prompt renders
/// at inference - the history holds neither the system prompt nor the tools.
///
/// `tools` is carried across for the same reason. A record without it is
/// rendered by a template that omits the tools preamble, so the model would be
/// trained on a prompt it never meets.
pub fn record_from_requests(
    family: &str,
    hidden_state: &str,
    requests: &[serde_json::Value],
    verdict: &Verdict,
) -> Result<Record, Excluded> {
    if !verdict.solved() {
        return Err(Excluded::NotSolved {
            unmet: verdict.failed().to_vec(),
        });
    }
    let last = requests.last().ok_or(Excluded::NothingToLearn)?;
    let wire = last
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or(Excluded::NothingToLearn)?;

    let mut messages = Vec::new();
    for message in wire {
        let role = message
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string();
        if role.is_empty() {
            return Err(Excluded::OpaqueTurn {
                role: "(none)".into(),
            });
        }
        let content = match message.get("content") {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Null) | None => String::new(),
            // A structured content block is a message this harness cannot
            // flatten without inventing a rendering for it.
            Some(_) => return Err(Excluded::OpaqueTurn { role }),
        };

        let tool_calls = message
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .map(|calls| {
                calls
                    .iter()
                    .map(|call| WireToolCall {
                        id: call.get("id").and_then(|i| i.as_str()).map(String::from),
                        kind: "function".into(),
                        function: WireFunction {
                            name: call
                                .pointer("/function/name")
                                .and_then(|n| n.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            arguments: call
                                .pointer("/function/arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("{}")
                                .to_string(),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        let train = role == "assistant";
        messages.push(WireMessage {
            role,
            content,
            tool_calls,
            tool_call_id: message
                .get("tool_call_id")
                .and_then(|i| i.as_str())
                .map(String::from),
            train,
        });
    }

    if !messages.iter().any(|m| m.train) {
        return Err(Excluded::NothingToLearn);
    }

    Ok(Record {
        messages,
        tools: last
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default(),
        metadata: RecordMetadata {
            family: family.to_string(),
            provenance: Provenance::Scripted,
            hidden_state: hidden_state.to_string(),
        },
    })
}

/// Build training records from a captured prompt and performed actions.
///
/// **The first decision only.** Not a simplification - the only shape this
/// checkpoint's chat template can be masked against.
///
/// The template renders an assistant turn conditionally on whether anything
/// follows it:
///
/// ```jinja
/// {%- if loop.index0 > ns.last_query_index %}
///     {%- if loop.last or (not loop.last and reasoning_content) %}
/// ```
///
/// so the same message produces different text in isolation than in context,
/// there is no honest boundary to mask at, and the trainer refuses it rather
/// than guessing. One record per decision does not help either: the earlier
/// decisions are still context in the later records and fail identically.
/// Only a record whose sole assistant turn is its last message is stable.
///
/// The template's own escape is a non-empty `reasoning_content`, which makes
/// both branches identical - but `generic-messages-v2` has no such field and
/// is `deny_unknown_fields`, so a producer cannot reach it. Multi-turn
/// trajectory SFT on this checkpoint is a masking problem on the engine side,
/// not something a dataset producer can work around.
///
/// This is not a consolation prize for this sample: the measured failure is
/// that the model acts without investigating, so the first decision is the one
/// that matters.
///
/// The system prompt and tool schemas come from a request the agent really
/// sent, so a record renders at training time the way the prompt renders at
/// inference. The observations come from the real tool executor. The
/// decisions, and only the decisions, are the demonstration's.
pub fn records_from_performance(
    family: &str,
    hidden_state: &str,
    prompt: &serde_json::Value,
    request: &str,
    performed: &[(String, String, String)],
    closing: &str,
    verdict: &Verdict,
) -> Result<Vec<Record>, Excluded> {
    if !verdict.solved() {
        return Err(Excluded::NotSolved {
            unmet: verdict.failed().to_vec(),
        });
    }
    if performed.is_empty() {
        return Err(Excluded::NothingToLearn);
    }

    let system = prompt
        .pointer("/messages/0/content")
        .and_then(|c| c.as_str())
        .ok_or(Excluded::OpaqueTurn {
            role: "system".into(),
        })?;
    let tools = prompt
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let metadata = || RecordMetadata {
        family: family.to_string(),
        provenance: Provenance::Scripted,
        hidden_state: hidden_state.to_string(),
    };

    // Context as it stood before the first decision: the prompt and the
    // request, nothing else. Anything more would put an assistant turn in the
    // context and make the record unmaskable.
    let (tool, arguments, _observation) = &performed[0];
    let messages = vec![
        context("system", system),
        context("user", request),
        WireMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: vec![WireToolCall {
                id: Some("call_0".into()),
                kind: "function".into(),
                function: WireFunction {
                    name: tool.clone(),
                    arguments: arguments.clone(),
                },
            }],
            tool_call_id: None,
            train: true,
        },
    ];
    let records = vec![Record {
        messages,
        tools,
        metadata: metadata(),
    }];
    let _ = closing;

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use sven_sdk::ToolCallRecord;

    use crate::PredicateSet;

    fn verdict(pass: bool) -> Verdict {
        let set = PredicateSet::new(["done"]).expect("one predicate");
        let observed: BTreeMap<String, bool> = [("done".to_string(), pass)].into_iter().collect();
        set.evaluate(&observed).expect("evaluated")
    }

    fn call(id: &str, name: &str, args: &str) -> ToolCallRecord {
        ToolCallRecord {
            id: id.into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    fn solved_transcript() -> Vec<Turn> {
        vec![
            Turn::System {
                text: "you are an agent".into(),
            },
            Turn::User {
                text: "enable retries".into(),
            },
            Turn::Assistant {
                text: String::new(),
                tool_calls: vec![call("c0", "shell", r#"{"command":"./svctl status"}"#)],
            },
            Turn::ToolResult {
                call_id: "c0".into(),
                text: "active deployment: staging".into(),
            },
            Turn::Assistant {
                text: "done".into(),
                tool_calls: Vec::new(),
            },
        ]
    }

    #[test]
    fn only_assistant_turns_are_supervised() {
        // Supervising a tool result teaches the model to produce tool output
        // itself - to invent the answer it was supposed to go and fetch.
        let record = record_from_episode(
            "f",
            "staging",
            Provenance::OnPolicy,
            &solved_transcript(),
            &verdict(true),
        )
        .expect("a solved episode yields a record");
        for message in &record.messages {
            assert_eq!(
                message.train,
                message.role == "assistant",
                "only assistant turns may be trained on, got {message:?}"
            );
        }
    }

    #[test]
    fn an_unverified_episode_yields_nothing() {
        let err = record_from_episode(
            "f",
            "staging",
            Provenance::OnPolicy,
            &solved_transcript(),
            &verdict(false),
        )
        .expect_err("an unsolved episode must not become training data");
        assert!(matches!(err, Excluded::NotSolved { .. }), "{err}");
    }

    #[test]
    fn a_transcript_with_a_hole_is_dropped_rather_than_emitted() {
        let mut transcript = solved_transcript();
        transcript.insert(
            2,
            Turn::Opaque {
                role: "user".into(),
            },
        );
        let err = record_from_episode(
            "f",
            "staging",
            Provenance::OnPolicy,
            &transcript,
            &verdict(true),
        )
        .expect_err("a transcript missing a step must not become an example");
        assert!(matches!(err, Excluded::OpaqueTurn { .. }), "{err}");
        assert!(err.to_string().contains("missing a step"));
    }

    #[test]
    fn several_calls_in_one_model_turn_become_one_assistant_message() {
        // Split across messages, the example would teach the model to stop
        // after the first call.
        let transcript = vec![
            Turn::User { text: "go".into() },
            Turn::Assistant {
                text: String::new(),
                tool_calls: vec![call("c0", "a", "{}")],
            },
            Turn::Assistant {
                text: String::new(),
                tool_calls: vec![call("c1", "b", "{}")],
            },
        ];
        let record = record_from_episode(
            "f",
            "staging",
            Provenance::Teacher,
            &transcript,
            &verdict(true),
        )
        .expect("a record");
        let assistants: Vec<_> = record
            .messages
            .iter()
            .filter(|m| m.role == "assistant")
            .collect();
        assert_eq!(assistants.len(), 1, "{:?}", record.messages);
        assert_eq!(assistants[0].tool_calls.len(), 2);
    }

    #[test]
    fn tool_arguments_survive_verbatim() {
        // Re-serialising would normalise key order and spacing away from what
        // the model actually emitted.
        let raw = r#"{"command":"./svctl status","timeout":5}"#;
        let transcript = vec![
            Turn::User { text: "go".into() },
            Turn::Assistant {
                text: String::new(),
                tool_calls: vec![call("c0", "shell", raw)],
            },
        ];
        let record = record_from_episode(
            "f",
            "staging",
            Provenance::OnPolicy,
            &transcript,
            &verdict(true),
        )
        .expect("a record");
        assert_eq!(record.messages[1].tool_calls[0].function.arguments, raw);
    }

    #[test]
    fn provenance_travels_with_the_record() {
        // A teacher trace is excellent supervised data and is not an
        // on-policy rollout. Forgetting which is which is how a claim about
        // self-improvement quietly stops being true.
        for provenance in [
            Provenance::OnPolicy,
            Provenance::Teacher,
            Provenance::Scripted,
        ] {
            let record = record_from_episode(
                "f",
                "staging",
                provenance,
                &solved_transcript(),
                &verdict(true),
            )
            .expect("a record");
            assert_eq!(record.metadata.provenance, provenance);
        }
    }

    #[test]
    fn a_record_serialises_as_one_jsonl_line_with_train_on_every_message() {
        let record = record_from_episode(
            "f",
            "staging",
            Provenance::OnPolicy,
            &solved_transcript(),
            &verdict(true),
        )
        .expect("a record");
        let jsonl = to_jsonl(&[record]).expect("serialises");
        assert_eq!(jsonl.lines().count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(jsonl.trim()).expect("valid json");
        for message in parsed["messages"].as_array().expect("messages") {
            assert!(
                message.get("train").is_some(),
                "every message needs train: {message}"
            );
        }
    }
}
