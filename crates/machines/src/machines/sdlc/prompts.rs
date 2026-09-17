// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Embedded per-state deliberation prompts for the SDLC machine.
//!
//! Each state issues **one comprehensive instruction** — a real command, not
//! raw JSON data — together with a state-scoped tool subset and the shared
//! decision schema.  The instruction frames the role, summarises the process
//! step, gives the explicit task, and tells the model to answer per the schema.
//!
//! The returned [`serde_json::Value`] is the opaque `request` of
//! [`Effect::CallLlm`](sven_hsm::Effect::CallLlm); it carries the
//! `kind: "deliberate"` discriminator so the deliberation executor routes it
//! (mirrors [`sven_llm::DeliberationRequest`]'s wire shape without a crate
//! dependency).

use serde_json::{json, Value};

use super::decisions::decision_schema;

/// Read-only investigation tools (discovery, planning, verification).
pub const READ_TOOLS: &[&str] = &[
    "read_file",
    "find_file",
    "grep",
    "search_codebase",
    "read_lints",
    "list_dir",
    "glob",
];

/// Mutating tools available during execution (plus the read tools).
pub const WRITE_TOOLS: &[&str] = &[
    "read_file",
    "find_file",
    "grep",
    "search_codebase",
    "read_lints",
    "list_dir",
    "glob",
    "edit_file",
    "delete_file",
    "shell",
    "run_terminal_command",
];

/// Build-and-test tools (verification / delivery).
pub const BUILD_TOOLS: &[&str] = &[
    "read_file",
    "grep",
    "search_codebase",
    "read_lints",
    "shell",
    "run_terminal_command",
];

/// Shared tail appended to every instruction: how to answer.
pub const ANSWER_CONTRACT: &str =
    "Respond with a single JSON object matching the decision schema. Set \
     `status` to exactly one of: `proceed` (you are confident and the phase is \
     complete), `need_user_input` (you must ask the developer something — put \
     the questions in `questions`), `need_approval` (you need the developer to \
     approve before continuing — describe it in `approval_prompt`), `need_tools` \
     (you still need to investigate further), or `failed` (you cannot proceed). \
     Put a concise human summary in `summary`, any user-facing text in \
     `message`, and structured results in `payload`. Do not output anything \
     except the JSON object.";

/// Shared tail appended to every instruction: how to answer.
pub fn answer_contract() -> &'static str {
    ANSWER_CONTRACT
}

/// Assemble a kernel-native turn request for an SDLC phase.
///
/// The `system_role` is prepended to the `instruction` as context so the model
/// understands its role without a separate system-message API parameter.
fn turn_request(
    thread: &str,
    system_role: &str,
    instruction: String,
    tools: &[&str],
    schema_name: &str,
    max_tool_rounds: u32,
) -> Value {
    let full_instruction = if system_role.is_empty() {
        instruction
    } else {
        format!("{system_role}\n\n{instruction}")
    };
    json!({
        "kind": "turn",
        "thread": thread,
        "instruction": full_instruction,
        "tools": tools,
        "schema": decision_schema(),
        "schema_name": schema_name,
        "max_tool_rounds": max_tool_rounds,
    })
}

/// Intake: classify intent and either chat / clarify, or confirm an actionable
/// scope.  Read-only tools so the model can peek at the repo when useful.
#[must_use]
pub fn intake_request(user_request: &str) -> Value {
    let instruction = format!(
        "You are a senior software consultant in the initial information-gathering \
         stage of an engineering engagement. The developer just said:\n\n\"{user_request}\"\n\n\
         Your job is to understand what (if anything) they actually want built or \
         changed. First decide whether this is real, actionable engineering work \
         or just chit-chat / a greeting / an unclear request.\n\
         - If it is a greeting or small talk, reply warmly in `message` and set \
           `status` to `need_user_input` with a question inviting them to describe \
           the task. DO NOT start any engineering work.\n\
         - If the request is actionable but under-specified, set `status` to \
           `need_user_input` and put the specific clarifying questions you need \
           answered in `questions`.\n\
         - If you clearly understand an actionable scope, summarise the problem, \
           intent, and constraints in `summary`/`payload`, and set `status` to \
           `need_approval` with an `approval_prompt` that restates the scope and \
           asks the developer to confirm before you begin discovery.\n\n{}",
        answer_contract()
    );
    turn_request(
        "intake",
        "You are Sven, a meticulous senior software engineer who never starts work \
         before understanding the request.",
        instruction,
        READ_TOOLS,
        "intake_decision",
        6,
    )
}

/// Follow-up turn on a thread after the developer answered a question.
#[must_use]
pub fn followup_request(thread: &str, system_role: &str, tools: &[&str], answer: &str) -> Value {
    let instruction = format!(
        "The developer responded:\n\n\"{answer}\"\n\n\
         Incorporate this and continue the current phase. {}",
        answer_contract()
    );
    turn_request(thread, system_role, instruction, tools, "decision", 6)
}

/// Generic follow-up after a rejected approval — ask the model to revise.
#[must_use]
pub fn revise_request(thread: &str, system_role: &str, tools: &[&str], reason: &str) -> Value {
    let instruction = format!(
        "The developer did NOT approve. Reason/context: \"{reason}\". Revise your \
         approach to address their concern and continue. {}",
        answer_contract()
    );
    turn_request(thread, system_role, instruction, tools, "decision", 6)
}

/// Discovery: explore the repository and produce a discovery summary.
#[must_use]
pub fn discovery_request(scope_summary: &str) -> Value {
    let instruction = format!(
        "You are in the discovery phase. The agreed scope is:\n\n{scope_summary}\n\n\
         Explore this project using the available read-only tools: determine how \
         it is structured, which components are relevant to the task, how it \
         builds and tests, and any constraints or risks. Accumulate your findings, \
         then produce a discovery summary in `summary` and structured findings in \
         `payload` (e.g. relevant files, build/test commands, risks). Set `status` \
         to `proceed` when discovery is complete, `need_user_input` if something \
         essential is missing, or `failed` if you cannot understand the project.\n\n{}",
        answer_contract()
    );
    turn_request(
        "discovery",
        "You are Sven performing repository discovery. Use tools to gather facts; \
         never guess when you can read.",
        instruction,
        READ_TOOLS,
        "discovery_decision",
        20,
    )
}

/// Planning: produce a candidate plan / task decomposition.
#[must_use]
pub fn planning_request(discovery_summary: &str) -> Value {
    let instruction = format!(
        "You are in the planning phase. Discovery established:\n\n{discovery_summary}\n\n\
         Produce a concrete, minimal implementation plan. Break the work into an \
         ordered list of atomic tasks; for each task give a short id, a \
         description, and the files likely involved. Put the plan and the task \
         list in `payload` (e.g. `{{\"tasks\": [{{\"id\": \"t1\", \"description\": \
         \"...\"}}]}}`) and a one-paragraph overview in `summary`. When the plan is \
         ready, set `status` to `need_approval` with an `approval_prompt` asking \
         the developer to approve the plan before execution. Use `need_user_input` \
         if you need a decision from the developer first.\n\n{}",
        answer_contract()
    );
    turn_request(
        "planning",
        "You are Sven, an engineer who plans minimal, low-risk changes and \
         decomposes work into small verifiable tasks.",
        instruction,
        READ_TOOLS,
        "planning_decision",
        16,
    )
}

/// Execution: implement the approved plan (single-track in Phase 1).
#[must_use]
pub fn execution_request(plan_summary: &str) -> Value {
    let instruction = format!(
        "You are in the execution phase. The approved plan is:\n\n{plan_summary}\n\n\
         Implement the tasks now using the available tools: read the relevant \
         code, apply minimal patches with the edit tools, and run builds/tests to \
         check your work. Work autonomously through the tasks. When all tasks are \
         implemented and the project builds, set `status` to `proceed` and \
         summarise what you changed in `summary` (and structured details in \
         `payload`). Use `need_approval` before any destructive or irreversible \
         action, `need_user_input` if you are blocked on a developer decision, or \
         `failed` if you cannot complete the work.\n\n{}",
        answer_contract()
    );
    turn_request(
        "execution",
        "You are Sven implementing changes carefully: smallest viable diffs, \
         always verify by building and testing.",
        instruction,
        WRITE_TOOLS,
        "execution_decision",
        40,
    )
}

/// A single decomposed task executed by a child submachine (Phase 2 fan-out).
///
/// Runs on its own isolated `task` thread with the write/build tool subset, so
/// each child accumulates a private append-only conversation that never touches
/// the parent's execution thread.
#[must_use]
pub fn task_request(task: &str) -> Value {
    let instruction = format!(
        "You are implementing ONE isolated task from a larger, already-approved \
         plan:\n\n\"{task}\"\n\n\
         Implement just this task using the available tools: read the relevant \
         code, apply minimal patches, and build/test to check your work. Do not \
         start work belonging to other tasks. When this task is complete and the \
         project builds, set `status` to `proceed` and summarise what you changed \
         in `summary` (structured details in `payload`). Use `failed` if you \
         cannot complete it.\n\n{}",
        answer_contract()
    );
    turn_request(
        "task",
        "You are Sven implementing one isolated task with the smallest viable diff.",
        instruction,
        WRITE_TOOLS,
        "task_decision",
        40,
    )
}

/// Verification: independently verify the implementation.
#[must_use]
pub fn verification_request(execution_summary: &str) -> Value {
    let instruction = format!(
        "You are in the verification phase. The implementation reported:\n\n\
         {execution_summary}\n\n\
         Independently verify the work: build the project, run the tests and any \
         static analysis, and check the changes satisfy the original requirements \
         without regressions. Put a pass/fail verdict and evidence in `payload` \
         and a summary in `summary`. Set `status` to `proceed` if verification \
         passes, `failed` if it does not (so the machine can recover), or \
         `need_user_input` if you need guidance.\n\n{}",
        answer_contract()
    );
    turn_request(
        "verification",
        "You are Sven acting as an independent reviewer; you trust evidence from \
         builds and tests over claims.",
        instruction,
        BUILD_TOOLS,
        "verification_decision",
        20,
    )
}

/// Delivery: summarise the work and request final sign-off.
#[must_use]
pub fn delivery_request(verification_summary: &str) -> Value {
    let instruction = format!(
        "You are in the delivery phase. Verification concluded:\n\n\
         {verification_summary}\n\n\
         Produce the final deliverable summary for the developer: what changed and \
         why, how to use or test it, and any follow-ups. Put the technical summary \
         and user instructions in `payload` and a concise overview in `summary`. \
         Set `status` to `need_approval` with an `approval_prompt` asking the \
         developer to accept the delivered work.\n\n{}",
        answer_contract()
    );
    turn_request(
        "delivery",
        "You are Sven wrapping up an engagement with a clear, honest handover.",
        instruction,
        READ_TOOLS,
        "delivery_decision",
        12,
    )
}

/// Recovery: ask the model to diagnose a failure and propose how to proceed.
#[must_use]
pub fn recovery_request(failure_context: &str) -> Value {
    let instruction = format!(
        "Something went wrong. Context:\n\n{failure_context}\n\n\
         Diagnose the failure and decide how to proceed. If a corrective retry is \
         viable, explain it in `summary` and set `status` to `proceed`. If you \
         need a decision from the developer, set `status` to `need_user_input` \
         with `questions`. If the situation is unrecoverable, set `status` to \
         `failed`.\n\n{}",
        answer_contract()
    );
    turn_request(
        "recovery",
        "You are Sven diagnosing a failure calmly and proposing the smallest safe \
         corrective action.",
        instruction,
        READ_TOOLS,
        "recovery_decision",
        12,
    )
}

/// The system-role string used for follow-up / revise turns on each thread.
#[must_use]
pub fn role_for_thread(thread: &str) -> &'static str {
    match thread {
        "intake" => {
            "You are Sven, a meticulous senior software engineer who never starts work \
             before understanding the request."
        }
        "discovery" => {
            "You are Sven performing repository discovery. Use tools to gather facts; \
             never guess when you can read."
        }
        "planning" => {
            "You are Sven, an engineer who plans minimal, low-risk changes and \
             decomposes work into small verifiable tasks."
        }
        "execution" => {
            "You are Sven implementing changes carefully: smallest viable diffs, \
             always verify by building and testing."
        }
        "verification" => {
            "You are Sven acting as an independent reviewer; you trust evidence from \
             builds and tests over claims."
        }
        "delivery" => "You are Sven wrapping up an engagement with a clear, honest handover.",
        _ => "You are Sven, a meticulous senior software engineer.",
    }
}

/// The tool subset used for follow-up turns on each thread.
#[must_use]
pub fn tools_for_thread(thread: &str) -> &'static [&'static str] {
    match thread {
        "execution" => WRITE_TOOLS,
        "verification" => BUILD_TOOLS,
        _ => READ_TOOLS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_turn_request(v: &Value, thread: &str) {
        assert_eq!(v["kind"], "turn", "SDLC prompts must use kind=turn");
        assert_eq!(v["thread"], thread);
        assert!(v["instruction"].as_str().unwrap().len() > 50);
        assert!(v["tools"].as_array().unwrap().len() >= 3);
        assert_eq!(v["schema"]["type"], "object");
    }

    #[test]
    fn intake_request_is_well_formed() {
        let v = intake_request("fix the auth bug");
        assert_turn_request(&v, "intake");
        assert!(v["instruction"].as_str().unwrap().contains("fix the auth bug"));
    }

    #[test]
    fn all_phase_requests_well_formed() {
        assert_turn_request(&discovery_request("scope"), "discovery");
        assert_turn_request(&planning_request("disc"), "planning");
        assert_turn_request(&execution_request("plan"), "execution");
        assert_turn_request(&verification_request("exec"), "verification");
        assert_turn_request(&delivery_request("ver"), "delivery");
    }

    #[test]
    fn execution_has_write_tools() {
        let v = execution_request("plan");
        let tools: Vec<&str> = v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(tools.contains(&"edit_file"));
    }

    #[test]
    fn followup_carries_answer() {
        let v = followup_request("intake", "role", READ_TOOLS, "the repo is at ./x");
        assert!(v["instruction"]
            .as_str()
            .unwrap()
            .contains("the repo is at ./x"));
    }
}
