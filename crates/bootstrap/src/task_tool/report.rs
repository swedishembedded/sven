// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! How a sub-agent's session ended, and what the `task` tool reports for it.

use agent_client_protocol::StopReason;

/// The tokens a sub-agent's turn used, as its server reported them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TurnTokens {
    pub(super) input: u64,
    pub(super) output: u64,
}

/// How a sub-agent's session ended.
#[derive(Debug)]
pub(super) enum SessionEnd {
    /// The handshake failed before a prompt was sent.
    Handshake(String),
    /// The child would not switch to the mode it was started for.
    ModeRefused(String),
    /// The prompt turn finished with this stop reason and usage.
    Finished(StopReason, Option<TurnTokens>),
    /// The prompt request itself failed.
    PromptFailed(String),
    /// The parent cancelled the tool call.
    Cancelled,
    /// The child went silent for longer than the inactivity timeout.
    Inactive,
    /// The contract's deadline passed.
    OutOfTime,
    /// The child process exited, or its stream closed, before its turn
    /// finished.
    ChildGone,
}

/// What a report says besides the end itself.
pub(super) struct ReportContext<'a> {
    pub(super) mode: &'a str,
    /// The wall-clock budget the child was given, in seconds.
    pub(super) budget_secs: u64,
    pub(super) handle_id: &'a str,
    pub(super) description: &'a str,
    /// The child's stderr, for an end that happened before any turn.
    pub(super) stderr: &'a str,
}

/// The tool's result for `end`: `Ok` with the report of a turn the child
/// finished, `Err` with why it did not. Only a completed turn is a success; a
/// child that stops or disappears before finishing never is.
pub(super) fn report(
    end: &SessionEnd,
    final_text: &str,
    ctx: &ReportContext<'_>,
) -> Result<String, String> {
    let ReportContext {
        mode,
        budget_secs,
        handle_id,
        description,
        stderr,
    } = ctx;
    let tag = format!("Handle: {handle_id}\nDescription: {description}");
    let stop = match end {
        SessionEnd::Handshake(msg) => return Err(format!("{msg}{stderr}")),
        SessionEnd::ModeRefused(msg) => {
            return Err(format!(
                "the sub-agent could not be put in '{mode}' mode and was stopped: {msg}{stderr}"
            ))
        }
        SessionEnd::Inactive => {
            return Err("sub-agent timed out after 10 minutes of inactivity".into())
        }
        SessionEnd::OutOfTime => {
            return Err(format!(
                "sub-agent ran out of its wall-clock budget ({budget_secs}s) and was stopped\n{tag}"
            ))
        }
        SessionEnd::Cancelled | SessionEnd::Finished(StopReason::Cancelled, _) => {
            return Err(format!("Sub-agent was cancelled.\n{tag}"))
        }
        SessionEnd::PromptFailed(msg) => return Err(format!("sub-agent failed: {msg}\n{tag}")),
        SessionEnd::ChildGone => {
            return Err(format!(
                "sub-agent exited before finishing its turn\n{tag}\n\nPartial result:\n{final_text}"
            ))
        }
        SessionEnd::Finished(stop, _) => *stop,
    };
    let stop_word = match stop {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        StopReason::Refusal => "refusal",
        _ => "unknown",
    };
    if matches!(stop, StopReason::MaxTokens | StopReason::Refusal) {
        return Err(format!(
            "Sub-agent stopped early: {stop_word}.\n{tag}\n\nPartial result:\n{final_text}"
        ));
    }
    let status_word = if stop == StopReason::EndTurn {
        "success"
    } else {
        "failed"
    };
    let body = if final_text.is_empty() {
        "(No assistant text produced.)".to_string()
    } else {
        format!("--- Result ---\n{final_text}")
    };
    Ok(format!(
        "Sub-agent completed ({status_word}, stop={stop_word}).\n{tag}\n\n{body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ReportContext<'static> {
        ReportContext {
            mode: "agent",
            budget_secs: 90,
            handle_id: "h",
            description: "d",
            stderr: "",
        }
    }

    #[test]
    fn only_a_finished_turn_is_a_success() {
        assert!(report(
            &SessionEnd::Finished(StopReason::EndTurn, None),
            "done",
            &ctx()
        )
        .is_ok());
        let gone = report(&SessionEnd::ChildGone, "half", &ctx()).unwrap_err();
        assert!(gone.contains("exited before finishing"), "{gone}");
        let late = report(&SessionEnd::OutOfTime, "", &ctx()).unwrap_err();
        assert!(late.contains("wall-clock budget (90s)"), "{late}");
        for end in [
            SessionEnd::Cancelled,
            SessionEnd::Inactive,
            SessionEnd::Finished(StopReason::MaxTokens, None),
        ] {
            assert!(report(&end, "", &ctx()).is_err(), "{end:?}");
        }
    }
}
