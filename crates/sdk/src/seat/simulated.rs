// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent played as the seat from a spec file.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::checks::{check_reply, SeatChecks};
use super::{Provenance, Seat, SeatReply};
use crate::{Agent, CallError, Engine};

/// A fact only the seat knows, and when it may be told.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenFact {
    /// Names the fact in a violation.
    pub id: String,
    /// The text that must not appear before it is unlocked.
    pub value: String,
    /// The fact is unlocked once the learner has said any of these.
    pub disclose_when: Vec<String>,
}

/// The raw materials a simulated seat is prepped with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatSpec {
    /// Names the spec in a provenance record.
    pub id: String,
    /// Who the seat plays and how they speak.
    pub persona: String,
    /// What the seat wants out of the conversation.
    pub goals: Vec<String>,
    /// The seat's first words, verbatim.
    pub opening: String,
    /// Facts the seat knows but only tells when asked.
    #[serde(default)]
    pub hidden_facts: Vec<HiddenFact>,
    /// What a good learner has to work out. Never shown to the seat's model:
    /// it exists only so a reply that states it anyway is caught as a leak.
    #[serde(default)]
    pub answer_key: Vec<String>,
    /// Phrases that mean the seat stopped playing its role.
    #[serde(default = "default_role_breaks")]
    pub role_breaks: Vec<String>,
    /// Recorded in the provenance for reproducible reruns.
    #[serde(default)]
    pub seed: u64,
}

fn default_role_breaks() -> Vec<String> {
    ["as an ai", "language model", "i am an assistant"]
        .map(String::from)
        .to_vec()
}

impl SeatSpec {
    /// Parses a spec file, returning it with the SHA-256 of its text.
    ///
    /// # Errors
    ///
    /// When the YAML is not a valid spec.
    pub fn parse(yaml: &str) -> Result<(Self, String), CallError> {
        let spec = serde_yaml::from_str(yaml)
            .map_err(|e| CallError::Precondition(format!("seat spec: {e}")))?;
        Ok((spec, format!("{:x}", Sha256::digest(yaml.as_bytes()))))
    }

    /// The system prompt of the seat's model: the persona, the goals and the
    /// disclosure rules, never the answer key.
    fn briefing(&self) -> String {
        let mut text = format!(
            "You play this person in a conversation. Stay in role, speak as them, \
             never mention being an AI.\n\nWho you are: {}\n\nWhat you want:\n",
            self.persona
        );
        for goal in &self.goals {
            text.push_str(&format!("- {goal}\n"));
        }
        if !self.hidden_facts.is_empty() {
            text.push_str(
                "\nThings you know but only tell when the other person asks about them \
                 (do not volunteer them):\n",
            );
            for fact in &self.hidden_facts {
                text.push_str(&format!(
                    "- {} (tell when asked about: {})\n",
                    fact.value,
                    fact.disclose_when.join(", ")
                ));
            }
        }
        text.push_str("\nReply with what you say next, nothing else. Say [END] when you are done.");
        text
    }
}

/// The marker a seat's model ends the conversation with.
const END_MARKER: &str = "[END]";

struct State {
    agent: Agent,
    learner_said: String,
    checks: SeatChecks,
}

/// A seat played by an agent of its own, from a [`SeatSpec`].
pub struct SimulatedSeat {
    spec: SeatSpec,
    spec_sha256: String,
    state: Mutex<State>,
}

impl SimulatedSeat {
    /// A seat whose model is `engine`'s, prepped with `spec`. `engine` is
    /// the seat's own, separate from the learner's, so neither sees the
    /// other's history.
    #[must_use]
    pub fn new(engine: &Engine, spec: SeatSpec, spec_sha256: String) -> Self {
        let agent = engine.agent_with_role("agent", spec.briefing());
        Self {
            spec,
            spec_sha256,
            state: Mutex::new(State {
                agent,
                learner_said: String::new(),
                checks: SeatChecks::default(),
            }),
        }
    }
}

#[async_trait]
impl Seat for SimulatedSeat {
    async fn open(&self) -> Result<SeatReply, CallError> {
        Ok(SeatReply {
            text: self.spec.opening.clone(),
            done: false,
        })
    }

    async fn reply(&self, learner: &str) -> Result<SeatReply, CallError> {
        let mut state = self.state.lock().await;
        state.learner_said.push('\n');
        state.learner_said.push_str(learner);
        let outcome = state.agent.send(learner).await?;
        let violations = check_reply(&self.spec, &state.learner_said, &outcome.reply);
        state.checks.record(violations);
        let done = outcome.reply.contains(END_MARKER);
        Ok(SeatReply {
            text: outcome.reply.replace(END_MARKER, "").trim().to_string(),
            done,
        })
    }

    fn provenance(&self) -> Provenance {
        Provenance::Simulated {
            spec_id: self.spec.id.clone(),
            spec_sha256: self.spec_sha256.clone(),
            seed: self.spec.seed,
        }
    }

    fn checks(&self) -> Option<SeatChecks> {
        self.state.try_lock().ok().map(|s| s.checks.clone())
    }
}
