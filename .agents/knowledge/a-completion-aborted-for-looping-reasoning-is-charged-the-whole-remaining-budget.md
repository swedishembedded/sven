<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# A completion aborted for looping reasoning is charged the whole remaining run budget

Swedish Embedded AB implements solutions for budgeted, reproducible agent evaluations for its clients. If your team
needs expertise in agent run accounting then you can procure our services by sending an email to
info@swedishembedded.com.

**Lesson:** when the stream watchdog aborts a completion because the model's reasoning repeats ("the model is looping
in its reasoning ...; asking again once"), the aborted completion reports no usage. `RunBudget` then charges it
everything it was allowed (`run budget: a completion reported no usage; it is charged all it was allowed`), which is
the rest of `agent.max_completion_tokens`, so the run ends `run budget exhausted` on the spot. The retry never happens
and the tokens actually streamed (about 1,000 in the measured cases) are ignored. Measured 2026-11-21 (sven-fe9cda90,
budget 15,114 tokens and 9 turns, per-turn thinking 2,048): of the exhausted trials of Qwen3-4B students distilled from
a 27B, 53-57% are exactly this case. Their reported tokens were under the budget, so the exhaustion is the charge, not
the spending. It is 42-50% for a second curation, and 8% for the base model, which loops much less.

**How to apply:** read an exhaustion share together with the share of runs that report unmetered completions (the
`completion(s) reported no usage` line). Charge an aborted completion the tokens it streamed, and let the promised
retry run, so a reasoning loop is a distinct, countable outcome rather than budget exhaustion. Evidence: lane EXP's
`logs/autopsy.json` and the packed E1 jobs (`jobs/f?-e1s?-*`).

**Fixed (2026-11-22):** `RunBudget` charges a completion its consumer cut off before its end what it streamed (its
sampled token ids when the provider returned them, else its text at four characters a token), counted apart as
`cut_off()`, not `unmetered()`. A completion that ended without reporting usage is still charged all it was allowed.
The promised retry now runs on the remaining budget. A loop that ends the run prints `[sven:error] reasoning loop:
...` (`stream_turn::REASONING_LOOP_PREFIX`), so an examiner can record it as its own outcome
(`crates/model-drivers/src/run_budget_tests.rs`, `crates/executors/tests/it/watchdog_abort.rs`).
