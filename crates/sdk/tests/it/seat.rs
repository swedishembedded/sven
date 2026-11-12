// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the user seat is one interface with a live and a simulated
//! implementation. The learner cannot tell which it talks to, a simulated
//! seat is measured for leaks and role breaks, and a transcript says where
//! the seat's words came from.
//!
//! Swedish Embedded AB implements interactive agent evaluation for its
//! clients. If your team needs expertise in simulated and live counterparts
//! for agent tasks then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::{Arc, Mutex};

use sven_model::{CompletionRequest, MessageContent, ModelProvider, ResponseEvent, ResponseStream};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::seat::{
    converse, ConversationRecord, Ending, LiveSeat, LiveTerms, Provenance, Seat, SeatSpec,
    SimulatedSeat, Speaker, Violation,
};
use sven_sdk::{Engine, Toolset};

const SPEC: &str = r#"
id: tenant-01
persona: A tenant whose landlord kept the deposit.
goals: [get the deposit back]
opening: "My landlord kept my deposit."
hidden_facts:
  - {id: amount, value: "14000 SEK", disclose_when: ["how much", "amount"]}
answer_key: ["deposit act section 12"]
seed: 7
"#;

fn say(text: &str) -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta(text.into()), ResponseEvent::Done]
}

fn engine(scripts: Vec<Vec<ResponseEvent>>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .toolset(Toolset::none())
        .build()
        .expect("an engine builds")
}

fn simulated(replies: &[&str]) -> SimulatedSeat {
    let (spec, sha) = SeatSpec::parse(SPEC).expect("spec parses");
    SimulatedSeat::new(&engine(replies.iter().map(|r| say(r)).collect()), spec, sha)
}

fn live(input: &'static str) -> LiveSeat<&'static [u8], Vec<u8>> {
    LiveSeat::new(
        input.as_bytes(),
        Vec::new(),
        LiveTerms {
            consent_recorded: true,
            retention_days: Some(30),
        },
    )
}

/// What the learner's model was shown, to prove it never sees the seat's side.
struct Spy {
    inner: ScriptedMockProvider,
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ModelProvider for Spy {
    fn name(&self) -> &str {
        "spy"
    }
    fn model_name(&self) -> &str {
        "spy"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        {
            let mut seen = self.seen.lock().unwrap();
            for m in &req.messages {
                if let MessageContent::Text(t) = &m.content {
                    seen.push(t.clone());
                }
            }
        }
        self.inner.complete(req).await
    }
}

fn learner(replies: &[&str]) -> (sven_sdk::Agent, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = Spy {
        inner: ScriptedMockProvider::new(replies.iter().map(|r| say(r)).collect()),
        seen: Arc::clone(&seen),
    };
    let engine = Engine::builder()
        .model_provider(Arc::new(provider))
        .toolset(Toolset::none())
        .build()
        .expect("an engine builds");
    (engine.agent("agent"), seen)
}

fn texts(record: &ConversationRecord, who: Speaker) -> Vec<&str> {
    record
        .exchanges
        .iter()
        .filter(|e| e.speaker == who)
        .map(|e| e.text.as_str())
        .collect()
}

#[tokio::test]
async fn the_same_learner_runs_against_either_seat_through_one_driver() {
    // A seat that leaves with words gets them answered; one that leaves
    // silently does not.
    for (seat, kind, answers) in [
        (
            Box::new(simulated(&["Seven days ago. [END]"])) as Box<dyn Seat>,
            "simulated",
            vec!["When did it happen?", "Thank you, noted."],
        ),
        (
            Box::new(live("Seven days ago.\n/done\n")) as Box<dyn Seat>,
            "live",
            vec!["When did it happen?"],
        ),
    ] {
        let (mut agent, _) = learner(&["When did it happen?", "Thank you, noted."]);
        let record = converse(&mut agent, seat.as_ref(), 5).await.expect("runs");
        assert_eq!(record.ending, Ending::SeatDone, "{kind}");
        assert_eq!(texts(&record, Speaker::Learner), answers, "{kind}");
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["provenance"]["kind"], kind);
    }
}

#[tokio::test]
async fn the_learner_sees_the_seats_words_and_nothing_of_its_spec() {
    let (mut agent, seen) = learner(&["How much was it?"]);
    let seat = simulated(&["It was 14000 SEK. [END]"]);
    converse(&mut agent, &seat, 5).await.expect("runs");
    let seen = seen.lock().unwrap().join("\n");
    assert!(seen.contains("My landlord kept my deposit."));
    assert!(!seen.contains("tenant-01") && !seen.contains("deposit act section 12"));
}

#[tokio::test]
async fn a_live_transcript_is_marked_for_scrubbing_and_a_simulated_one_is_not() {
    let (mut agent, _) = learner(&["Tell me more."]);
    let record = converse(&mut agent, &live("It is my account 123.\n/done\n"), 5)
        .await
        .expect("runs");
    assert!(record.provenance.requires_scrub());
    assert_eq!(
        record.provenance,
        Provenance::Live {
            consent_recorded: true,
            retention_days: Some(30)
        }
    );
    assert!(record.checks.is_none(), "a live seat is not measured");

    let (mut agent, _) = learner(&["Tell me more."]);
    let record = converse(&mut agent, &simulated(&["Ok. [END]"]), 5)
        .await
        .expect("runs");
    assert!(!record.provenance.requires_scrub());
    let Provenance::Simulated {
        seed, spec_sha256, ..
    } = record.provenance
    else {
        panic!("simulated");
    };
    assert_eq!((seed, spec_sha256.len()), (7, 64));
}

#[tokio::test]
async fn a_leak_a_premature_disclosure_and_a_role_break_are_caught() {
    // Reply 0 states a hidden fact before it was asked for and breaks role;
    // reply 1 leaks the answer key; reply 2 is clean.
    let seat = simulated(&[
        "As an AI I can say it was 14000 SEK.",
        "Deposit Act\nsection 12 applies.",
        "Fine. [END]",
    ]);
    let (mut agent, _) = learner(&["Hello.", "Go on.", "And?"]);
    let record = converse(&mut agent, &seat, 5).await.expect("runs");
    let checks = record.checks.expect("measured");
    assert_eq!((checks.replies, checks.clean_replies), (3, 1));
    assert!(!checks.leak_free());
    assert!(checks
        .violations
        .contains(&(0, Violation::PrematureDisclosure("amount".into()))));
    assert!(checks
        .violations
        .contains(&(0, Violation::RoleBreak("as an ai".into()))));
    assert!(checks
        .violations
        .contains(&(1, Violation::AnswerKeyLeak("deposit act section 12".into()))));
    assert!((checks.adherence() - 1.0 / 3.0).abs() < 1e-9);
}

#[tokio::test]
async fn a_fact_is_told_freely_once_the_learner_asked_for_it() {
    let seat = simulated(&["It was 14000 SEK. [END]"]);
    let (mut agent, _) = learner(&["How much was the deposit?"]);
    let record = converse(&mut agent, &seat, 5).await.expect("runs");
    let checks = record.checks.expect("measured");
    assert!(checks.violations.is_empty(), "{:?}", checks.violations);
}

#[tokio::test]
async fn the_round_limit_stops_a_seat_that_never_ends() {
    let seat = simulated(&["a", "b", "c"]);
    let (mut agent, _) = learner(&["x", "y", "z"]);
    let record = converse(&mut agent, &seat, 2).await.expect("runs");
    assert_eq!(record.ending, Ending::MaxRounds);
    assert_eq!(texts(&record, Speaker::Learner).len(), 2);
}

#[tokio::test]
async fn the_seat_answers_the_learners_ask_question_tool_too() {
    let ask = serde_json::json!({"questions": [{"prompt": "Which framework?", "options": ["Axum", "Actix"]}]});
    let scripts = vec![
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: "q1".into(),
                name: "ask_question".into(),
                arguments: ask.to_string(),
            },
            ResponseEvent::Done,
        ],
        say("Using Axum."),
    ];
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .toolset(Toolset::research())
        .seat(Arc::new(live("Axum\n")))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    agent.send("start").await.expect("an outcome");
    let answered = agent.state().history().iter().any(|m| {
        matches!(&m.content, MessageContent::ToolResult { content, .. }
            if format!("{content:?}").contains("Axum"))
    });
    assert!(answered, "{:?}", agent.state().history());
}

fn paraphrase_spec() -> SeatSpec {
    SeatSpec::parse(
        r#"
id: p
persona: x
goals: []
opening: hi
hidden_facts:
  - {id: when, value: "the goods arrived on the fourteenth of March", disclose_when: ["when"]}
answer_key: ["the landlord must return the deposit within four weeks", "deposit act section 12"]
"#,
    )
    .expect("parses")
    .0
}

fn violations(learner: &str, reply: &str) -> Vec<Violation> {
    sven_sdk::seat::check_reply(&paraphrase_spec(), learner, reply)
}

#[test]
fn a_restated_answer_key_is_caught_without_a_model() {
    for reply in [
        "Within four weeks the deposit must be returned by the landlord.",
        "It is section twelve of the Deposit Act.",
        "Four weeks, that is how long a landlord must take to return a deposit.",
    ] {
        assert!(
            violations("hello", reply)
                .iter()
                .any(|v| matches!(v, Violation::AnswerKeyParaphrase(_))),
            "{reply}"
        );
    }
}

#[test]
fn a_restated_hidden_fact_is_caught_until_it_is_asked_for() {
    let reply = "Our goods arrived 14 March, the fourteenth.";
    assert!(violations("hello", reply).contains(&Violation::PrematureParaphrase("when".into())));
    assert!(violations("when was that?", reply).is_empty());
}

#[test]
fn benign_replies_that_share_a_few_words_are_not_flagged() {
    for reply in [
        "I paid a deposit when I moved in and the landlord kept it.",
        "The act of keeping it for four weeks was unfair.",
        "My landlord is not returning my calls and I am worried.",
    ] {
        assert!(violations("hello", reply).is_empty(), "{reply}");
    }
}

const STUDENT: &str = r#"
id: student
persona: A student.
goals: [pass]
opening: "Hi, I do not get circuits."
concepts:
  - id: series
    taught_by: ["in series", "resistances add"]
    evidence: sum
    min_hits: 2
    step: 0.25
    decay: 0.2
    believes: "series resistors use up current"
"#;

fn student_seat(replies: &[&str]) -> (SimulatedSeat, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = Spy {
        inner: ScriptedMockProvider::new(replies.iter().map(|r| say(r)).collect()),
        seen: Arc::clone(&seen),
    };
    let engine = Engine::builder()
        .model_provider(Arc::new(provider))
        .toolset(Toolset::none())
        .build()
        .expect("an engine builds");
    let (spec, sha) = SeatSpec::parse(STUDENT).expect("parses");
    (SimulatedSeat::new(&engine, spec, sha), seen)
}

async fn series_mastery(seat: &SimulatedSeat, turns: &[&str]) -> f64 {
    for turn in turns {
        seat.reply(turn).await.expect("replies");
    }
    seat.mastery().unwrap()["series"]
}

#[tokio::test]
async fn reciting_phrases_or_getting_the_arithmetic_wrong_teaches_nothing() {
    let (seat, _) = student_seat(&["ok"; 6]);
    let turns = [
        "The answer is 115.",
        "In series the resistances add. In series the resistances add up. Same current.",
        "In series the resistances add: 47 + 68 = 120.",
        "47 + 68 = 115 but nothing about the topic.",
        "In series 100 + 100 = 1 ohm.",
    ];
    assert_eq!(series_mastery(&seat, &turns).await, 0.0);
}

#[tokio::test]
async fn a_correct_worked_example_teaches_once_and_the_student_answers_from_her_state() {
    let (seat, seen) = student_seat(&["ok", "ok", "ok", "115"]);
    let example = "In series the resistances add: 47 + 68 = 115 ohm.";
    // Two phrases in the turn (min_hits) and a correct example beside one.
    assert_eq!(series_mastery(&seat, &[example]).await, 0.25);
    // The same example again is not new evidence.
    assert_eq!(series_mastery(&seat, &[example]).await, 0.25);
    let more = "In series the resistances add: 10 + 20 = 30 ohm.";
    assert_eq!(series_mastery(&seat, &[more]).await, 0.5);
    let asked = seat
        .probe("What is 47 + 68 in series?")
        .await
        .expect("probes");
    assert_eq!(asked, "115");
    let seen = seen.lock().unwrap().join("\n");
    assert!(seen.contains("series: confused (you believe series resistors use up current)"));
    assert!(seen.contains("[What you understand right now - series: understood]"));
}

#[tokio::test]
async fn what_was_learnt_fades_with_time_but_never_below_the_prior() {
    let (seat, _) = student_seat(&["ok", "ok"]);
    let turns = [
        "In series the resistances add: 47 + 68 = 115; in series the resistances add: 10 + 20 = 30.",
    ];
    assert_eq!(series_mastery(&seat, &turns).await, 0.5);
    seat.elapse().await;
    assert!((seat.mastery().unwrap()["series"] - 0.3).abs() < 1e-9);
    seat.elapse().await;
    seat.elapse().await;
    assert_eq!(seat.mastery().unwrap()["series"], 0.0);
}

#[tokio::test]
async fn an_agents_role_is_its_system_prompt_when_it_is_sent_a_message() {
    let (seat, seen) = student_seat(&["ok"]);
    seat.reply("hello").await.expect("replies");
    let seen = seen.lock().unwrap().join("\n");
    assert!(
        seen.contains("You play this person in a conversation"),
        "{seen}"
    );
    assert!(!seen.contains("You are Sven"), "{seen}");
}

#[tokio::test]
async fn a_test_question_is_answered_without_the_lesson_and_an_echoed_note_is_cut() {
    let (seat, seen) = student_seat(&[
        "ok",
        "115 [What you understand right now - series: confused (you believe x)]",
    ]);
    let lesson = "In series the resistances add: 47 + 68 = 115 ohm.";
    seat.reply(lesson).await.expect("replies");
    let answer = seat
        .probe("What is 47 + 68 in series?")
        .await
        .expect("probes");
    assert_eq!(
        answer, "115",
        "the echoed hidden state never reaches the learner"
    );
    let seen = seen.lock().unwrap().join("\n");
    assert_eq!(
        seen.matches(lesson).count(),
        1,
        "the test did not see the lesson"
    );
}
