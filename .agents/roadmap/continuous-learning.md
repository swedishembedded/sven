# continuous-learning

**Status: planned, not started. This is sven's half of a cross-repo
initiative — the entry point and the full cross-repo picture (MVP cut,
dependency DAG, deferred work with justification) live in the orchestrator
repo's own `.agents/roadmap/continuous-learning.md`. brain's half is in
`edgeai/brain/.agents/roadmap/continuous-learning.md`. Read the
orchestrator's file first.**

(This is sven's first `.agents/roadmap/` entry — the directory didn't exist
before this file. Follow this file's format for future entries: what's the
goal, what's actually left, nothing else, per the sibling repos' own convention
which sven is adopting here.)

## Goal, restated for this repo

Sven's job in the loop: detect when the agent (or the user) is missing
information or facing a real choice, resolve it (ask the user, or — gated —
search the web, or accept a document the user hands over directly), tag
every resulting fact with honest provenance, and hand confirmed knowledge
off to a training service — without ever letting untrusted content reach
the model's weights, or even its live context, without the right level of
human sign-off. Extraction of facts/probes from a document is *sven's* job
(reasoning), not brain's (brain does math) and not a new machine or state —
it happens inside the existing `ReactiveAgentMachine` tool loop.

## Milestones

### S1 — make `semantic_memory` real, without breaking the `minimal` build
Today `sven-memory`'s SQLite+FTS5 `semantic_memory` tool is fully dead code —
`grep -rn SqliteMemoryStore` finds zero constructors outside its own crate,
and `RuntimeBuilder::build_tool_registry` (`crates/bootstrap/src/
runtime_builder.rs:557`) always passes `IntegrationProviders::default()`
(`registry.rs:136`), which never builds one. Making it default-on is not a
one-line flip: `rusqlite` is in `xtask`'s `FORBIDDEN_IN_MINIMAL` list
(`xtask/src/main.rs:527`), and `make check` runs `cargo run -p xtask --
arch --profile minimal` before clippy. Split a new `memory` feature out of
the monolithic `integrations` feature (`crates/bootstrap/Cargo.toml:42-46`),
add it to `sven-bootstrap`'s and the root crate's `default`, and leave
`minimal = []` untouched.

**Test-first:**
- `semantic_memory_is_present_in_the_default_registry` — red today, it never
  is;
- run and read `cargo run -p xtask -- arch --profile minimal` — this is the
  actual gate, not optional verification;
- a bats case in `tests/e2e/basic/` for headless remember/recall.
**Commit boundary: two** — (i) split the feature (pure plumbing, minimal
profile still green — this half is safe for a cheap model *if and only if*
it actually runs the minimal-profile check), (ii) construct the real store
and register the tool by default.

### S3′ — a bounded clarification check in the default agent mode
`ReactiveAgentMachine` (the default mode) has **zero** clarification
mechanism today — only `SdlcMachine` has a `NeedUserInput` decision status
(`crates/machines/src/machines/sdlc/decisions.rs:24-53`), and porting that
pattern would force every free-form default-mode turn through structured
JSON parsing, a real regression.

**The cheap, correct MVP version, after rejecting two more expensive
designs:** no new confidence signal (rejected — see "Deferred" below), no
new machine state (rejected — `ReactiveAgentMachine`'s documented shape is
"`Generating` owns the full tool loop, no separate states",
`reactive_agent.rs:60-72`, and a new state contradicts it). Instead: a
bounded post-check on the existing `FinalAnswer` branch
(`reactive_agent.rs:221-232`) that — when the answer looks genuinely
unresolved (the model itself says so, or a request is ambiguous enough that
`ask_question`'s existing multiple-choice shape, `Question { prompt,
options, allow_multiple }`, `crates/tools-agent/src/ask_question.rs:16-21`,
is the obviously right next step) — emits one more `CallTool` and stays in
`Generating`, bounded by the existing `DEFAULT_MAX_TOOL_ROUNDS = 16`
(`:54`). No new event field. No brain dependency. This covers both
Clarification 1's asks — "clarify what they want", "pick alternatives" —
with a tool that already exists and already supports both.

**Test-first:**
- `a_low_confidence_final_answer_emits_one_ask_question_and_stays_in_
  generating`;
- `the_clarification_post_check_cannot_exceed_max_tool_rounds`;
- `an_unambiguous_final_answer_is_byte_identical_to_todays_behaviour` — the
  no-regression guard, since this touches the default mode every user hits.
**Commit:** one.

### S4 — `assimilate_fact`: the security milestone
Always writes to `semantic_memory` (instant recall this session), but only
appends to the durable pending-facts ledger (built on `sven-chain` —
foundation tier, hash-chained append-only JSONL, a legal `domain →
foundation` edge from `sven-memory`) when the fact's provenance clears an
admissibility rule enforced **at the kernel permission layer, never in
prompt text or tool arguments**.

**`FactSource`, precisely — this is the actual design, not a sketch:**

```rust
enum FactSource {
    UserStated,
    UserProvidedDocument { digest: ContentDigest, uri: String, ingested_at: u64, span: Range<usize> },
    WebSourced          { url: String, fetched_at: u64, digest: ContentDigest },
    AgentInferred       { from: Vec<FactId> },
    UserChoice          { question_id: String, chosen: String, not_chosen: Vec<String> },
}
```

| variant | ledger-admissible? | why |
|---|---|---|
| `UserStated` | yes | unchanged from the original design |
| `UserProvidedDocument` | **yes, no per-fact confirmation** | the human act of handing over the artifact *is* the approval; one approval covers exactly the facts whose `digest` matches — see `F1`'s "autonomously" requirement below |
| `WebSourced` | only with a genuine kernel `HumanApproved` event | autonomous web exploration stays gated — unchanged |
| `AgentInferred` | **no — memory only, never the ledger** | inference-closure admission ("admit if every source it derives from is admissible") is a real design with its own failure modes; out of MVP scope entirely |
| `UserChoice` | yes | the user's own act; captured for later use, see `S7` |

`UserProvidedDocument` must be its own variant, not folded into
`UserStated`: the approval *scope* differs. `UserStated` is per-utterance;
`UserProvidedDocument` is per-digest and covers N facts the user never
individually read line-by-line. Collapsing them destroys the ledger's
ability to answer "which document, and did a human actually approve *that*
document" — and it must not become `WebSourced` either, or `F1`'s "learn
from this document" instruction could never run autonomously (a document
the user directly hands over is categorically not the same trust level as
content the agent went and fetched on its own).

**The approval itself is never a tool parameter.** A boolean the LLM can set
is a suggestion, not a gate. `WebSourced` confirmation is set by the
executor from a real `HumanApproved` event — the same path `loop_core::
handle_tool_event` already uses for `ToolApprovalRequired` — which requires
a new `ToolCapability::AssimilateKnowledge`, added per this repo's own "Add
a `ToolCapability`" checklist (AGENTS.md): the enum + `is_inherently_
dangerous` + the classify path in `hsm/src/permissions.rs`, **every**
machine's `permission_policy()` (`reactive_agent.rs` and `sdlc/mod.rs`), and
the tool's own `kernel_capability()`.

**Test-first, five, including the two that matter most:**
- `a_user_provided_document_fact_reaches_the_ledger_without_a_per_fact_
  approval`;
- `a_web_sourced_fact_still_requires_a_human_approved_event`;
- `a_document_fact_whose_digest_does_not_match_any_approved_document_is_
  refused` — stops laundering arbitrary web content through a forged
  document record;
- **`a_model_supplied_confirmed_flag_is_ignored`** — pass `confirmed: true`
  in the tool call's own arguments, assert it still doesn't reach the
  ledger unless the executor independently observed a `HumanApproved`
  event;
- **`a_model_supplied_source_field_is_ignored`** — `source` is set by the
  ingestion/resolution path (`ask_question`'s answer, `web_fetch`'s URL,
  `ingest_document`'s digest — see `S5`/`S7`), never by the LLM's tool-call
  arguments. These two tests are the actual security boundary; everything
  else in this milestone exists to make them true.
**Commit:** one — `hsm/src/permissions.rs` + every `permission_policy()` +
the tool's `kernel_capability()` genuinely only compile together, so this
is one commit per AGENTS.md's "Add a `ToolCapability`" checklist, not a
place to force an artificial split.

### S4b — provenance-aware recall (closes a hole `S4` alone leaves open)
`S4` gates the *ledger* (what reaches training) but was originally specified
to write *every* source, unconditionally, into `semantic_memory` — and
`semantic_memory` is recalled straight into the model's prompt context. That
means an attacker-controlled page the agent fetches enters the model's
context on the very next recall with **no approval of any kind** — a
persistent prompt-injection path, sitting right next to a milestone whose
entire stated purpose is to gate untrusted content. `WebSourced`/
`AgentInferred` records must render into the prompt as **quoted, clearly-
untrusted content**, never as flat assertions the model treats as ground
truth, and an *unconfirmed* `WebSourced` record must stay session-scoped —
never durably recalled into a second, unrelated session.

**Test-first:**
- `a_web_sourced_memory_record_is_recalled_as_quoted_untrusted_content_
  never_as_an_assertion`;
- `an_unconfirmed_web_record_is_not_visible_to_a_second_session`.
**Commit:** one — separate from `S4`; this is the recall path, not the
permission bucket, and the two are independently reviewable.

### S5 — resolving tools attach provenance, they never write memory
`web_fetch`/`web_search`/`ask_question` attach provenance metadata (`url`,
`fetched_at`, `content_digest` for the web tools; `UserStated` for
`ask_question`) to their `ToolOutput`. **They do not write `semantic_
memory` or the ledger themselves** — `assimilate_fact` (`S4`) is the only
writer. One writer, one gate; two writers is how a gate gets bypassed by
accident six months from now.

**Test-first:** per-tool metadata-attachment tests; and
`assimilate_fact_refuses_a_web_sourced_record_whose_url_is_absent` — a
fabricated or missing provenance claim is a refusal, not a default.
**Commit boundary: two** — (i) the tools carry provenance metadata,
(ii) the machine's follow-up call into `assimilate_fact`.

### S6′ — the pending-facts drain, bidirectional
Reads the `sven-chain` ledger tail past a persisted cursor, batches, hands
the batch to a `FactSubmitter` trait, advances the cursor only after the
submitter reports **outcomes**, not just acceptance. A submit-only trait
(the original design) gives exactly-once *submission* and zero-once
*feedback* — useless the moment `F1` needs to know which facts in a batch
actually landed (`B8` in brain's file reports this per-fact; this milestone
is what carries that report back to sven and the user).

Runs as a background task in `sven-frontend` (wiring tier) behind a config
flag — **not** an HSM `Effect`; the HSM must not own a training pipeline,
per this repo's own separation of pure transitions from impure I/O. The
concrete orchestrator-speaking `FactSubmitter` implementation lives outside
this repo (`W7`), never here, per that repo's own "never leak
marketplace-shaped logic upstream" rule — this trait is deliberately generic
and marketplace- agnostic.

Reused, not reinvented: `sven-scheduler`'s due-job poller has zero call
sites anywhere in the binary (dead-code-adjacent — its `JobStore` is a
whole-file-rewrite YAML store shaped for "run an agent prompt on a cron",
not "hand N ledger entries to a training submitter"), and adopting it would
mean making a dead subsystem live *and* bending its payload, plus dragging
`serde_yaml`/`dirs` into the default build. `sven-chain` is the right
ledger already; this milestone is a small, purpose-built drain on top of it
(~120 lines), explicitly not a reuse of the scheduler crate.

**Honest limitation, stated in the code, not discovered later:**
`sven-chain`'s own module doc says it is not tamper-evident against an
attacker with local write access — it gives ordering, provenance, and
crash-safety, not integrity against local compromise.

**Test-first:** `a_restart_mid_drain_resubmits_no_fact_twice_and_loses_no_
outcome` — simulate a crash between submission and outcome, assert the
cursor logic neither double-submits nor drops the eventual result.
**Commit boundary: two** — (i) the drain + persisted cursor + the generic
bidirectional `FactSubmitter` trait, (ii) wiring it into `sven-frontend`
behind a config flag.

**Done** (`c7d697e`, `0422ec1`). Both halves landed as specified. What they
left open is that nothing implemented the trait, which `S8` closes.

### S8 — the local submitter, and a synchronous flush
`S6′` left a generic trait with no implementation: the remote one (`W7`) was the
only one planned, and that path is now a deliberately paused optional scale-out
rather than the path the loop runs on. So the primary submitter is a local
one — sven and brain on one machine, nothing leaving it — and the drain
finally has something to submit to.

**`LocalFactSubmitter`** (`crates/memory/src/local_study.rs`) writes the
batch as brain's `{fact, probe_question, expected_answer}` JSONL, runs
brain's gated document study as a subprocess with `--adapter-dir` pointed at
the directory this machine's `brain serve --watch-adapters DIR` polls, and
parses the JSON report back into the drain's per-fact verdicts. A promoted
adapter therefore reaches the *running* model with no restart — brain's
watcher is the other half.

Three things this milestone had to decide, all of them stated in the code:

- **Facts had no probes.** `S7`'s extraction discipline says the fact is
  trained and the probe is scored, but `PendingFactRecord` carried only the
  fact, so nothing could score anything. `assimilate_fact` now captures an
  optional frozen `probe_question`/`expected_answer` pair (half a probe, or
  a probe question that appears inside its own training row, is a loud
  refusal — the second mirrors brain's own `FactBatch` validation). A fact
  with no probe is still recorded and comes back `Rejected` for being
  unscoreable; inventing a probe from the fact would test the invention.
- **The trait is bidirectional, so the submitter needs durable memory.** A
  journal beside the datasets: the claim (which facts, which study
  directory) is fsynced before the subprocess starts, every verdict before
  it is returned. `outcomes_for` answers from it and never runs a second
  study. **Honest limitation:** a claim with no report is an interrupted
  study, and whether it already published an adapter is not knowable from
  here — so it is `Failed`, not retried. Conservative against the
  double-training the ledger exists to prevent.
- **The invocation is a config template, not a literal.** sven depends on
  the *shape* — a base checkpoint and a dataset in, an adapter directory and
  a report out — and keeps the spelling in
  `tools.memory.learning.study_args` (`{weights}`, `{dataset}`,
  `{adapter_dir}`, `{report}`), so a renamed subcommand or one of brain's
  optional study knobs (`--lora`, `--steps`, `--lr`, `--eval-per-cycle`,
  `--seed`) is a config line rather than a sven release. The default is read
  off brain's own `document-study` usage: it is top-level and `--arch`-driven
  because the study machinery is generic over the model.

**The synchronous flush.** The drain's background task is right for an open
TUI session and useless for `sven --headless "learn from this document"` in
a shell script, which exits before the next tick. `PendingFactsDrain::
drain_all` keeps passing until the ledger is genuinely settled — *not* until
a pass returns no reports, since the pass that clears a stranded in-flight
marker settles nothing and stopping there would silently skip the fact it
just disclaimed — and `sven learn flush` is its CLI surface: blocks on real
outcomes, never a sleep, one line per fact, non-zero exit only when a fact
`failed` (a rejection is a real answer). Additive: the periodic drain is
untouched and the interactive path is byte-identical.

**Selection is config**, not a compile-time choice —
`tools.memory.learning.submitter` (`local` by default, `none`), which is the
concrete point of `S6′`'s trait being generic. `sven-frontend::
spawn_default_fact_drain` is the background half of the same selection.

**Test-first:** `a_frozen_probe_travels_into_the_ledger_beside_its_fact`,
`a_studied_batch_becomes_one_verdict_per_fact`,
`a_fact_already_studied_is_answered_from_the_journal_not_studied_again`,
`the_study_invocation_is_a_template_sven_only_substitutes_paths_into`,
`a_flush_settles_every_pending_fact_before_it_returns`,
`a_flush_does_not_mistake_a_disclaimed_batch_for_an_empty_ledger`.
**Commit boundary: three** — (i) probe capture, (ii) the submitter + its
config, (iii) `drain_all` + `sven learn flush` + the default wiring.

Three inputs the `local` submitter refuses to guess, each with its own
error naming the config key: `adapter_dir` (a wrong one trains a model
nothing serves), `base_weights` (an adapter trained over a different base is
not applicable to what is running), and `anchors_file` — the behavioural
anchor suite `Regime::Sft` mixes into every cycle's draw, which brain
refuses to run without and which is the operator's to state, since what a
deployment must never forget is a property of that deployment.

**Still open:** no test has run against a real `brain` binary. sven's
dataset shape, argv and report parsing were written against brain's actual
`document-study` source (top-level, `--arch`-driven, the
`{cycles, anchors}` dataset its `deny_unknown_fields` decoder accepts, and
the `gated.cycles[]` report it writes), but that command was still
uncommitted in brain's working tree — so the integration test drives a stub
`brain` and pins sven's half of the contract only. When it lands: re-check
the default `study_args` and the report field names against the committed
version, and add a real end-to-end case.

**Per-fact granularity is a cycle-level verdict, deliberately.** brain
reports per-CYCLE numbers, because the gate's decision IS a per-cycle
measurement and reconstructing per-fact verdicts would need a second decode
pass — a different measurement from the gate's, presented as if it were the
gate's. So facts handed over in one drain pass share a verdict, and a
rejection carries the gate's own cause and the cycle's probe pass rates
rather than invented per-fact ones. brain's roadmap `B8`
(`promote::document::fact_verdicts`) is the finer-grained answer when it
reaches the report.

### S7 — document ingestion + preference-choice capture
Two related but separable additions:

**`ingest_document`**: reads a file/path the user hands over, computes its
digest (the tool computes this — never asserted by the model, closing the
same "model-supplied field" hole `S4`'s tests guard against), and records a
`DocumentRecord { digest, uri, ingested_at }` in the `sven-chain` ledger.
This digest is what `S4`'s `UserProvidedDocument` admissibility check
matches against. This is the entry point for `F1`'s "learn what you can
from this document" instruction, and it's the only new tool `F1` needs —
extraction itself (turning document content into `{fact, probe_question,
expected_answer}` triples) happens inside the existing `ReactiveAgentMachine`
tool loop via repeated `assimilate_fact` calls, not a new machine or a new
brain capability. Extraction discipline mirrors brain's own: the fact is
what gets trained, the probe is what gets scored, and a probe's expected
answer must never appear inside the fact meant to train it — probes are
frozen at extraction time.

**`UserChoice` capture**: when `ask_question`'s multiple-choice shape
(`Question.options`, `ask_question.rs:16-21`) is used to let the user pick
between alternatives, record **both** the chosen option and the rejected
ones (`not_chosen`) via the `UserChoice` variant above — not just the
answer. This is deliberately captured now and trained on later (see
"Deferred" below): an uncaptured choice is gone forever the moment the
session ends, while training on it later costs nothing extra, because
brain's DPO objective (`crates/rl/src/objective/dpo.rs`) already exists and
is gradchecked — the only reason not to train on it in the MVP is that
deciding *which* user interactions produce a valid preference pair (an
agent-authored multiple-choice pick clearly does; a free-form typed answer
does not, because there's no `rejected` side) is a real design question,
and it isn't on `F1`'s critical path. Recording `not_chosen` is the entire
reason to touch this now rather than after the MVP — a ledger holding only
the chosen answer is worthless for DPO later.

**Test-first:**
- `ingest_document_computes_its_own_digest_never_trusts_a_model_supplied_
  one`;
- `a_user_choice_records_both_the_chosen_and_the_not_chosen_options`.
**Commit boundary: two** — different tools, different concerns:
(i) `ingest_document`, (ii) `UserChoice` capture on `ask_question`'s
existing multiple-choice path.

## Deferred, roadmapped, not lost

- **`S2` (a real per-turn confidence signal, entropy over top-K logprobs,
  computed in `sven-turn` from real distribution data — never a scalar a
  provider just asserts).** Rejected for MVP, deliberately, not just
  postponed by default: it requires brain's `B6` decoder-surface change
  (the live serving path returns token-ids only today, no logits to compute
  entropy from) *and* a cross-cutting change through 5+ sven crates
  (`sven-model`'s `ResponseEvent`, 34 driver impls, `sven-turn`'s
  `stream_turn`, `sven-executors`, `sven-hsm`'s `Event` — with this repo's
  own AGENTS.md noting the `wildcard_enum_match_arm` lint that would catch a
  missed renderer was never actually wired in, making this a manual
  four-surface checklist). `F1`'s signal is a programmatic verifier's
  pass/fail on frozen probes, not the model's self-reported confidence — it
  gets zero value from this, on a tiny CPU fixture whose entropy wouldn't
  even be calibrated. `S3′` above is the MVP substitute: cheaper, uses
  infrastructure that already exists and is already proven, and covers the
  actual user-facing need (clarify, or pick an alternative) without it.
- **Training on `UserChoice` signals via brain's DPO objective** — capture
  is in (`S7`), training is out. See `S7`'s reasoning above.
- **Admitting `AgentInferred` facts to the ledger via an inference-closure
  rule** ("admit if every fact it derives from is itself admissible") — a
  real idea, a real design surface, not scoped for MVP.
