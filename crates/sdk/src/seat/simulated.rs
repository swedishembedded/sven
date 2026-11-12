// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent played as the seat from a spec file.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::checks::{check_reply, mentions, unlocked, SeatChecks};
use super::evidence::{self, Evidence};
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

/// One thing the seat can understand or be confused about, with the
/// deterministic rule that moves it.
///
/// Mastery starts at `mastery`. It rises by `step` (to at most 1) for each new
/// worked example the learner states: a sentence containing a `taught_by`
/// phrase and three numbers that satisfy the concept's [`Evidence`] relation
/// (at most two new examples per turn, each distinct example counted once). A
/// turn must also contain at least `min_hits` distinct `taught_by` phrases.
/// Reciting phrases, stating answers, or getting the arithmetic wrong moves
/// nothing, so keyword stuffing earns no mastery. Mastery gained in the
/// conversation (not the prior) falls by `decay` for each time-passing event
/// ([`Seat::elapse`], e.g. unrelated questions between tests). The seat is
/// told with every message whether it understands the concept (mastery at
/// least `known_at`) or still believes `believes`.
///
/// The rule is a deliberately simple, inspectable proxy; how far a model
/// playing the student departs from it is measured, not assumed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Concept {
    /// Names the concept in the state note and the record.
    pub id: String,
    /// Mastery before the conversation, 0 to 1.
    #[serde(default)]
    pub mastery: f64,
    /// Phrases (case-insensitive) that mark a sentence as about the concept.
    pub taught_by: Vec<String>,
    /// The relation a worked example must satisfy.
    pub evidence: Evidence,
    /// How many distinct phrases a turn needs.
    #[serde(default = "one")]
    pub min_hits: usize,
    /// Mastery gained per new worked example.
    #[serde(default = "half")]
    pub step: f64,
    /// Mastery at which the seat understands.
    #[serde(default = "half")]
    pub known_at: f64,
    /// Mastery lost per [`Seat::elapse`], never below the prior.
    #[serde(default)]
    pub decay: f64,
    /// What the seat believes while it does not understand.
    pub believes: String,
}

fn one() -> usize {
    1
}

fn half() -> f64 {
    0.5
}

impl Concept {
    /// The number of new verified worked examples in `learner`, recorded in `seen`.
    fn learnt_from(&self, learner: &str, seen: &mut BTreeSet<String>) -> usize {
        let text = learner.to_lowercase();
        let phrases: Vec<String> = self.taught_by.iter().map(|p| p.to_lowercase()).collect();
        if phrases.iter().filter(|p| text.contains(p.as_str())).count() < self.min_hits {
            return 0;
        }
        evidence::new_examples(self.evidence, &phrases, learner, seen)
    }
}

/// The raw materials a simulated seat is prepped with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// What the seat can learn, for a seat that plays a student. Empty for a
    /// seat whose knowledge does not change.
    #[serde(default)]
    pub concepts: Vec<Concept>,
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
        if !self.concepts.is_empty() {
            text.push_str(
                "\nEvery message ends with a note on what you understand right now. Answer \
                 strictly from it: where it says you understand, answer correctly; where you \
                 are confused, answer from the belief it gives, with confidence. You know \
                 nothing beyond the note.\n",
            );
        }
        text.push_str("\nReply with what you say next, nothing else. Say [END] only when the conversation has reached its natural end and you have answered everything you were asked.");
        text
    }
}

/// The marker a seat's model ends the conversation with.
/// The start of the note [`SimulatedSeat`] appends to every message. A model
/// sometimes echoes it; the learner must never read the seat's hidden state.
const NOTE_MARKER: &str = "[What you understand right now";

/// `reply` up to any echo of the state note.
fn without_note(reply: &str) -> &str {
    reply.find(NOTE_MARKER).map_or(reply, |at| &reply[..at])
}

const END_MARKER: &str = "[END]";

struct State {
    agent: Agent,
    mastery: BTreeMap<String, f64>,
    seen: BTreeMap<String, BTreeSet<String>>,
    told: BTreeSet<String>,
    learner_said: String,
    checks: SeatChecks,
}

/// A seat played by an agent of its own, from a [`SeatSpec`].
pub struct SimulatedSeat {
    engine: Engine,
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
        let mastery = spec
            .concepts
            .iter()
            .map(|c| (c.id.clone(), c.mastery))
            .collect();
        Self {
            engine: engine.clone(),
            spec,
            spec_sha256,
            state: Mutex::new(State {
                agent,
                mastery,
                seen: BTreeMap::new(),
                told: BTreeSet::new(),
                learner_said: String::new(),
                checks: SeatChecks::default(),
            }),
        }
    }
}

impl SimulatedSeat {
    /// Moves the seat's mastery by what the learner just said.
    fn learn(&self, state: &mut State, learner: &str) {
        for concept in &self.spec.concepts {
            let seen = state.seen.entry(concept.id.clone()).or_default();
            let gained = concept.learnt_from(learner, seen);
            if gained > 0 {
                let mastery = state.mastery.entry(concept.id.clone()).or_insert(0.0);
                *mastery = (*mastery + concept.step * gained as f64).min(1.0);
            }
        }
    }

    /// `message` followed by the note on what the seat understands now;
    /// `message` alone for a seat with no concepts.
    fn with_note(&self, state: &State, message: &str) -> String {
        if self.spec.concepts.is_empty() {
            return message.to_string();
        }
        let parts: Vec<String> = self
            .spec
            .concepts
            .iter()
            .map(|c| {
                if state.mastery[&c.id] >= c.known_at {
                    format!("{}: understood", c.id)
                } else {
                    format!("{}: confused (you believe {})", c.id, c.believes)
                }
            })
            .collect();
        format!("{message}\n\n{NOTE_MARKER} - {}]", parts.join("; "))
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
        self.learn(&mut state, learner);
        let message = self.with_note(&state, learner);
        let outcome = state.agent.send(&message).await?;
        let reply = without_note(&outcome.reply);
        let violations = check_reply(&self.spec, &state.learner_said, reply);
        state.checks.record(violations);
        for fact in &self.spec.hidden_facts {
            if mentions(reply, &fact.value) {
                state.told.insert(fact.id.clone());
            }
        }
        // A seat that walks out while a fact the learner asked for is untold
        // would score the learner for a conversation the seat cut short.
        let pending = self.spec.hidden_facts.iter().any(|f| {
            unlocked(&state.learner_said, &f.disclose_when) && !state.told.contains(&f.id)
        });
        let ends = reply.contains(END_MARKER);
        if ends && pending {
            state.checks.premature_ends += 1;
        }
        let done = ends && !pending;
        Ok(SeatReply {
            text: reply.replace(END_MARKER, "").trim().to_string(),
            done,
        })
    }

    /// A test question is answered by a fresh instance of the persona that
    /// knows only the note on what it understands now, not the conversation:
    /// a model that read the lesson would otherwise answer from the lesson,
    /// and the test would measure its reading instead of the modelled state.
    /// Without concepts the seat answers inside the conversation.
    async fn probe(&self, question: &str) -> Result<String, CallError> {
        let state = self.state.lock().await;
        if self.spec.concepts.is_empty() {
            drop(state);
            return Ok(self.reply(question).await?.text);
        }
        let message = self.with_note(&state, question);
        let mut fresh = self.engine.agent_with_role("agent", self.spec.briefing());
        let outcome = fresh.send(&message).await?;
        Ok(without_note(&outcome.reply).trim().to_string())
    }

    fn provenance(&self) -> Provenance {
        Provenance::Simulated {
            spec_id: self.spec.id.clone(),
            spec_sha256: self.spec_sha256.clone(),
            seed: self.spec.seed,
        }
    }

    async fn elapse(&self) {
        let mut state = self.state.lock().await;
        for concept in &self.spec.concepts {
            let mastery = state.mastery.entry(concept.id.clone()).or_insert(0.0);
            *mastery = (*mastery - concept.decay).max(concept.mastery);
        }
    }

    fn mastery(&self) -> Option<BTreeMap<String, f64>> {
        let state = self.state.try_lock().ok()?;
        (!state.mastery.is_empty()).then(|| state.mastery.clone())
    }

    fn checks(&self) -> Option<SeatChecks> {
        self.state.try_lock().ok().map(|s| s.checks.clone())
    }
}
