// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sample-agent-seat`: an agent works a task with a counterpart that is
//! either a person at the terminal or a simulated user played from a spec, and
//! the conversation is recorded with its provenance and the seat's checks.
//!
//! Optional probes (a test the counterpart takes before and after the
//! conversation) are put to the seat outside the conversation, so a harness
//! that grades them measures what the counterpart learned.
//!
//! Swedish Embedded AB implements interactive agent evaluation for its
//! clients. If your team needs expertise in simulated and live counterparts
//! for agent tasks then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sven_sdk::config::Config;
use sven_sdk::seat::{
    converse, ConversationRecord, LiveSeat, LiveTerms, Seat, SeatSpec, SimulatedSeat,
};
use sven_sdk::Engine;

/// A question put to the counterpart outside the conversation.
#[derive(Clone, Debug, Deserialize)]
pub struct Probe {
    /// Names the probe in the output.
    pub id: String,
    /// What the counterpart is asked.
    pub text: String,
}

/// The counterpart's answer to a [`Probe`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeAnswer {
    /// The probe's id.
    pub id: String,
    /// What the counterpart answered.
    pub answer: String,
}

/// What a run produces.
#[derive(Debug, Serialize, Deserialize)]
pub struct Output {
    /// The conversation, with provenance and the seat's checks.
    pub record: ConversationRecord,
    /// Answers given before the conversation.
    pub probes_before: Vec<ProbeAnswer>,
    /// Answers given after it.
    pub probes_after: Vec<ProbeAnswer>,
}

/// Which counterpart the learner works with.
pub enum SeatChoice<'a> {
    /// A simulated user: the spec file and the configuration of its model.
    Simulated {
        /// The `SeatSpec` YAML.
        spec: &'a Path,
        /// The configuration naming the seat's model.
        config: Box<Config>,
    },
    /// The person at the terminal.
    Live(LiveTerms),
}

/// Runs `learner` against the chosen counterpart for at most `rounds`.
///
/// # Errors
///
/// When a spec or probe file cannot be read, or the engine, seat or learner
/// fails.
pub async fn run(
    learner_config: Config,
    learner_role: Option<&str>,
    seat: SeatChoice<'_>,
    probes: &[Probe],
    rounds: usize,
) -> anyhow::Result<Output> {
    let learner_engine = Engine::builder().config(learner_config).build()?;
    let mut learner = match learner_role {
        Some(role) => learner_engine.agent_with_role("agent", role),
        None => learner_engine.agent("agent"),
    };
    let seat: Box<dyn Seat> = match seat {
        SeatChoice::Simulated { spec, config } => {
            let yaml = std::fs::read_to_string(spec)
                .with_context(|| format!("reading {}", spec.display()))?;
            let (spec, sha) = SeatSpec::parse(&yaml)?;
            let engine = Engine::builder().config(*config).build()?;
            Box::new(SimulatedSeat::new(&engine, spec, sha))
        }
        SeatChoice::Live(terms) => Box::new(LiveSeat::new(
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stderr(),
            terms,
        )),
    };
    let probes_before = ask(seat.as_ref(), probes).await?;
    let record = converse(&mut learner, seat.as_ref(), rounds).await?;
    let probes_after = ask(seat.as_ref(), probes).await?;
    Ok(Output {
        record,
        probes_before,
        probes_after,
    })
}

async fn ask(seat: &dyn Seat, probes: &[Probe]) -> anyhow::Result<Vec<ProbeAnswer>> {
    let mut answers = Vec::new();
    for probe in probes {
        answers.push(ProbeAnswer {
            id: probe.id.clone(),
            answer: seat.probe(&probe.text).await?,
        });
    }
    Ok(answers)
}
