//! Completion guards.
//!
//! Completion is NEVER "the LLM said done". It is a deterministic predicate
//! over [`Context`] facts set by the machine's own transitions. The single
//! source of truth is [`development_complete`].

use sven_hsm::Context;

/// Returns `true` when the software-development lifecycle has reached a
/// provably-complete state: every task is done, the test suite passed, and the
/// human signed off on the result.
///
/// All three conditions must hold simultaneously; none can be spoofed by the
/// LLM (they are only written by the machine on real [`ToolSucceeded`] /
/// [`HumanApproved`] events).
///
/// [`ToolSucceeded`]: sven_hsm::Event::ToolSucceeded
/// [`HumanApproved`]: sven_hsm::Event::HumanApproved
#[must_use]
pub fn development_complete(ctx: &Context) -> bool {
    let all_tasks_done = ctx
        .facts
        .get("all_tasks_done")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tests_passed = ctx
        .facts
        .get("tests_passed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let human_acceptance_obtained = ctx
        .facts
        .get("human_acceptance_obtained")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    all_tasks_done && tests_passed && human_acceptance_obtained
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::Context;

    #[test]
    fn all_flags_false_is_incomplete() {
        let ctx = Context::new();
        assert!(!development_complete(&ctx));
    }

    #[test]
    fn partial_flags_are_incomplete() {
        let mut ctx = Context::new();
        ctx.set_fact("all_tasks_done", true);
        ctx.set_fact("tests_passed", true);
        assert!(!development_complete(&ctx));
    }

    #[test]
    fn all_flags_true_is_complete() {
        let mut ctx = Context::new();
        ctx.set_fact("all_tasks_done", true);
        ctx.set_fact("tests_passed", true);
        ctx.set_fact("human_acceptance_obtained", true);
        assert!(development_complete(&ctx));
    }

    #[test]
    fn only_human_acceptance_is_incomplete() {
        let mut ctx = Context::new();
        ctx.set_fact("human_acceptance_obtained", true);
        assert!(!development_complete(&ctx));
    }
}
