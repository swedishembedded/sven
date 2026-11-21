<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# generic-learning

Scope: sven owns enforcement of the token and turn budget, forking from a recorded state with a named context block removed, and per-completion context provenance: the agent half of the generic learning loop. Items GL-S1 to GL-S3 and the sven-side stop list.

Source: the 2026-11-21 training-methods review (read-only advisory). The same review is recorded as `generic-learning.md` in the sibling repositories of the stack; the full plan E0-E5 is in the campaign driver's ledger.

## The generic loop and its invariants

The loop needs only the verifier, the tools, a token budget and state forks, and routes each task by its measured pass rate under the current policy, never by task content. Every repo holds the same four rules:

1. **Same observation.** A token is a training target only if it was sampled in a context the student will also see. Privileged sources (a teacher, a hinted self, an oracle solution) supply states, not text.
2. **Reasoning length is a budget.** A trial passes only if the verifier passes within `B` completion tokens and `N` turns. Native reasoning mode, one mode and one budget for teacher capture, training data and every exam arm; length is controlled by the budget and by lowest-cost selection among verified passes, never by prompt text.
3. **Evaluation that can see the effect.** At least 60 tasks over at least 20 families, a reference arm between about 0.15 and 0.85, three tiers (unseen instance, held-out variant, held-out family; only the third supports a transfer claim), wall clock an infrastructure backstop.
4. **No string matching.** No filter reads the prose of a trace or the content of a task.

## Capabilities

Landed 2026-11-21; what each guarantees is described where it is used:

- GL-S1, the run budget (`agent.max_completion_tokens`, `agent.max_turns`; typed `budget_exhausted` outcome, exit 4, the budget in force on every request-log line): `docs/reference/configuration.md#run-budget`.
- GL-S2, forking from a recorded trajectory without a named context block (`--drop-context-block`, `agent.drop_context_blocks`): `docs/guides/request-replay.md#forking-without-a-context-block`.
- GL-S3, per-completion context provenance (`context_blocks` on every ledger record of a trajectory): `docs/internals/token-ledger.md`.

Open:

| Id | Gap | Acceptance |
|---|---|---|
| GL-S2b | A fork starts from a whole trajectory; choosing state `s_t` inside it means giving `--load-trace` a trajectory cut at that step. A block that is not a system-prompt section (the dynamic suffix, a message) cannot be dropped. | `--load-trace` takes a step index; a named non-section block can be dropped |
| GL-S4 | A person-guided session (`sven --interactive PROMPT`) recorded with the fidelity of a headless run; partial: since 06b2ecc5 the TUI sends what a headless run sends (append, sampling, time line) and marks typed turns `extra.sven.origin: human`; its `--record-requests` log is the headless format. Open: its ATIF trace is folded from the chat view, so steps lack per-request `metrics`, `extra.sven.timing`, `tool_definitions`, `final_metrics` and the outcome stamp the headless `StepAssembler` writes; and it offers `ask_question`, which a headless run does not | the trace of an interactive run of a scripted mock task equals the headless trace of the same turns step for step, apart from the `human` steps |
| GL-S1b | Side-by-side sub-agents are each handed the whole remainder, so together they can take the parent past its bound (it stops at its next completion); a sub-agent whose turn fails reports no spending. | children share one reservation; a failed child's spending is still charged |

## Stop doing (sven)

- Turning thinking off as the default for traces (`enable_thinking = false`); length is the budget's job (the campaign driver's non-thinking-traces rule is superseded pending E2).
- Per-benchmark prompt text as the way to shape reasoning.
