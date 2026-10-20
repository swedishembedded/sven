// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for unattended agent runs whose
// outcome can be scored and audited, for its clients. If your team needs
// expertise in headless agent pipelines you can procure our services by
// sending an email to info@swedishembedded.com.

//! How a finished headless run is scored: the conclusion its exit code
//! claims, the verdict a verified-task run recorded, and the exit code such a
//! run earns.

use sven_hsm::context::Context as KernelContext;
use sven_machines::{ERROR_FACT, NEEDS_HUMAN_FACT, VERDICT_FACT};
use sven_session_store::{RunConclusion, Verdict};

use crate::output::{write_progress, write_stderr};
use crate::runner::{
    EXIT_AGENT_ERROR, EXIT_BUDGET_EXHAUSTED, EXIT_NEEDS_HUMAN, EXIT_STOPPED_WITHOUT_CONCLUSION,
    EXIT_SUCCESS, EXIT_TIMEOUT,
};

/// Map this runner's exit code onto the conclusion the reward scorer expects.
///
/// `EXIT_SUCCESS` is a claim, not a verdict: an aborted run also exits 0 here,
/// and [`OutcomeFold::conclude`] vetoes it from the event stream.
pub(super) fn conclusion_for(exit: i32) -> RunConclusion {
    match exit {
        EXIT_SUCCESS => RunConclusion::Success,
        EXIT_TIMEOUT => RunConclusion::Timeout,
        EXIT_BUDGET_EXHAUSTED => RunConclusion::BudgetExhausted,
        EXIT_NEEDS_HUMAN => RunConclusion::Waiting,
        EXIT_STOPPED_WITHOUT_CONCLUSION => RunConclusion::StoppedWithoutConclusion,
        _ => RunConclusion::AgentError,
    }
}

/// Reads the terminal verdict the verified-task machine recorded in its
/// final `Context`, if any.
///
/// Deliberately reads only [`VERDICT_FACT`] - a plain `{"passed": bool}` the
/// machine sets exactly once, at `Done`, only after a real
/// `Event::VerificationComplete`. A run that ended any other way (parked at
/// `Parked`, or a setup error recorded under [`ERROR_FACT`]) has no such fact,
/// so this returns `None` and `OutcomeFold::conclude` honestly reports
/// `SessionOutcome::Unknown` rather than a guessed pass or fail.
pub(super) fn verdict_from_context(ctx: &KernelContext) -> Option<Verdict> {
    if let Some(reason) = ctx.fact(ERROR_FACT).and_then(|v| v.as_str()) {
        write_stderr(&format!(
            "[sven:warn] verified-task run ended without a verdict: {reason}"
        ));
    }
    if let Some(needs_human) = ctx.fact(NEEDS_HUMAN_FACT) {
        write_progress(&format!(
            "[sven:info] verified-task run parked, needs a human: {needs_human}"
        ));
    }
    match ctx
        .fact(VERDICT_FACT)
        .and_then(|v| v.get("passed"))
        .and_then(|v| v.as_bool())
    {
        Some(true) => Some(Verdict::Passed),
        Some(false) => Some(Verdict::Failed),
        None => None,
    }
}

/// The exit code for a finished verified-task run, from its final `Context`.
///
/// `Parked` reuses [`EXIT_NEEDS_HUMAN`] exactly as an `Event::QuestionAsked`
/// park would - both mean "a human must decide, do not score this run yet".
/// A recorded verdict (pass or fail) is [`EXIT_SUCCESS`] either way: the
/// *run* completed and produced a real, stamped answer, and the stamped reward - not the process exit code - is
/// what carries whether that answer was a pass.
pub(super) fn exit_code_for_verified_task(ctx: &KernelContext) -> i32 {
    if ctx.fact(NEEDS_HUMAN_FACT).is_some() {
        EXIT_NEEDS_HUMAN
    } else if ctx.fact(VERDICT_FACT).is_some() {
        EXIT_SUCCESS
    } else {
        EXIT_AGENT_ERROR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_from_context_reads_only_the_verdict_fact() {
        let mut ctx = KernelContext::new();
        assert_eq!(
            verdict_from_context(&ctx),
            None,
            "no verdict fact yet -> unknown"
        );

        ctx.set_fact(VERDICT_FACT, serde_json::json!({"passed": true}));
        assert_eq!(verdict_from_context(&ctx), Some(Verdict::Passed));

        ctx.set_fact(VERDICT_FACT, serde_json::json!({"passed": false}));
        assert_eq!(verdict_from_context(&ctx), Some(Verdict::Failed));
    }

    #[test]
    fn exit_code_for_verified_task_maps_parked_to_needs_human() {
        let mut ctx = KernelContext::new();
        ctx.set_fact(
            NEEDS_HUMAN_FACT,
            serde_json::json!({"question": "?", "options": []}),
        );
        assert_eq!(exit_code_for_verified_task(&ctx), EXIT_NEEDS_HUMAN);
    }

    #[test]
    fn exit_code_for_verified_task_maps_a_recorded_verdict_to_success_either_way() {
        let mut ctx = KernelContext::new();
        ctx.set_fact(VERDICT_FACT, serde_json::json!({"passed": false}));
        assert_eq!(
            exit_code_for_verified_task(&ctx),
            EXIT_SUCCESS,
            "a real failed verdict is a completed run, not a process error"
        );
    }

    #[test]
    fn exit_code_for_verified_task_with_neither_fact_is_an_agent_error() {
        let ctx = KernelContext::new();
        assert_eq!(exit_code_for_verified_task(&ctx), EXIT_AGENT_ERROR);
    }

    #[test]
    fn verdict_from_context_ignores_a_parked_run() {
        let mut ctx = KernelContext::new();
        ctx.set_fact(
            NEEDS_HUMAN_FACT,
            serde_json::json!({"question": "?", "options": []}),
        );
        assert_eq!(
            verdict_from_context(&ctx),
            None,
            "a parked run must never be read as a graded outcome"
        );
    }

    #[test]
    fn conclusion_for_maps_every_exit_code() {
        assert_eq!(conclusion_for(EXIT_SUCCESS), RunConclusion::Success);
        assert_eq!(conclusion_for(EXIT_TIMEOUT), RunConclusion::Timeout);
        assert_eq!(
            conclusion_for(EXIT_BUDGET_EXHAUSTED),
            RunConclusion::BudgetExhausted
        );
        assert_eq!(conclusion_for(EXIT_AGENT_ERROR), RunConclusion::AgentError);
        assert_eq!(conclusion_for(EXIT_NEEDS_HUMAN), RunConclusion::Waiting);
        assert_eq!(
            conclusion_for(EXIT_STOPPED_WITHOUT_CONCLUSION),
            RunConclusion::StoppedWithoutConclusion
        );
        // Anything unexpected is a failure, never a silent success.
        assert_eq!(conclusion_for(42), RunConclusion::AgentError);
    }
}
