// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven questions` - the operator side of the async question-parking
//! primitive: list what is parked, and durably record a human's answer to
//! one.
//!
//! Deliberately does **not** resume the session that asked the question.
//! Recording the answer here only makes it available to whatever later reads
//! this ledger to resume a kernel session (`Event::HumanAnswered`) - that
//! resumption path does not exist yet. Answering here is still worth doing on
//! its own: it is the durable record a resumer will eventually need, and it
//! is what a human actually did, whether or not anything reads it back today.

use sven_hsm::QuestionId;
use sven_memory::{QuestionAnsweredRecord, QuestionLedger};

use crate::cli::QuestionsCommands;

pub(crate) fn run_questions_command(cmd: &QuestionsCommands) -> anyhow::Result<()> {
    match cmd {
        QuestionsCommands::List { json } => list(*json),
        QuestionsCommands::Answer { question_id, answer } => answer_question(question_id, answer),
    }
}

fn list(json: bool) -> anyhow::Result<()> {
    let ledger = QuestionLedger::at_default_path();
    let pending = ledger.pending_questions()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&pending)?);
        return Ok(());
    }

    if pending.is_empty() {
        println!("Nothing parked: every recorded question already has an answer.");
        return Ok(());
    }
    for q in &pending {
        let opts = if q.options.is_empty() {
            String::new()
        } else {
            format!("  [{}]", q.options.join(" / "))
        };
        println!("{}  {}{opts}", q.question_id.as_uuid(), q.prompt);
    }
    println!("{} question(s) parked.", pending.len());
    Ok(())
}

fn answer_question(question_id: &str, answer: &str) -> anyhow::Result<()> {
    let uuid = question_id
        .parse::<uuid::Uuid>()
        .map_err(|e| anyhow::anyhow!("{question_id:?} is not a valid question id: {e}"))?;
    let question_id = QuestionId::from_uuid(uuid);

    let ledger = QuestionLedger::at_default_path();
    ledger.record_answered(&QuestionAnsweredRecord {
        question_id,
        answer: answer.to_string(),
        answered_at: u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0),
    })?;
    println!(
        "Recorded. Resuming the session that asked this question is not wired up yet - \
         the answer is durable but nothing has read it back."
    );
    Ok(())
}
