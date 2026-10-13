# Decision runtime

A decision model is a small, non-generative model that answers typed questions
about a state: which of these options fits, how likely is this proposition.
`sven-decision` is the provider-agnostic contract between the harness and such
a model.

## Advisory, not authoritative

The decision model **only advises**. Deterministic policy and the state
machine stay authoritative:

- A decision point declares a [policy](#policy) before it asks anything, and
  an answer is used only if that policy accepts it. Every other outcome is an
  explicit abstention with a reason, and the caller does exactly what it did
  before the decision point existed (or nothing, per the policy's fallback).
- An answer names one of the options the caller itself offered. It carries no
  permission, approval or capability, and the crate cannot name those types
  (`architecture.toml` forbids a path from `sven-decision` to `sven-hsm` and
  the tool crates). A decision point that touches security or an irreversible
  action is **advisory-only by construction**: its accepted verdicts are
  flagged `advisory_only`, and the permission policy decides regardless.

## Contract

| type | role |
|------|------|
| `DecisionRequest { state, questions, deadline }` | typed questions about one state |
| `Question::Choice { id, instruction, options[{id, description?}] }` | pick one option |
| `Question::Probability { id, proposition }` | how likely a proposition is |
| `Question::Score { id, instruction, levels }` | place on an ordered scale |
| `DecisionResult` | model identity (id, checkpoint), latency, truncation and unsupported-kind flags, one `QuestionAnswer` per question |
| `Distribution` | the **full** ranked distribution of a question, best first, ties by the caller's order |
| `DecisionProvider` | async trait: `decide(&DecisionRequest) -> Result<DecisionResult, DecisionError>` |

`DecisionRequest::validate` refuses a request outside the published limits
(1 to 64 questions, 1 to 255 options per choice, 2 to 10 score levels,
unique non-empty ids, a 1 MiB state, a non-zero deadline) before anything is
sent.

## Error classes

| class | meaning |
|-------|---------|
| `unreachable` | the provider could not be reached |
| `model_not_installed` | the provider is up, the model is not installed on it |
| `failed_to_load` | installed, but could not be loaded |
| `unsupported_capability` | the model cannot answer what was asked |
| `timeout` | no answer within the deadline |
| `invalid_response` | the provider answered with something that is not a decision |
| `invalid_request` | the request violates the contract (a caller defect, never retried) |

## Policy

Every decision point declares a `DecisionPolicy`; there is no global
threshold.

| field | meaning |
|-------|---------|
| `label` | names the decision point in diagnostics and trajectories |
| `shape` | the one question kind allowed and its most options |
| `max_input_tokens` | most state the decision point sends (capped by what the served model reports) |
| `false_positive`, `false_negative` | what a wrong answer costs (`low`, `medium`, `high`) |
| `thresholds` | `min_confidence` (the winner's probability, or the model's own confidence when it reports one) and `min_margin` (the lead over the runner-up) |
| `impact` | `routine`, `security`, `irreversible`; anything but `routine` is advisory-only |
| `fallback` | `legacy` (behave as before) or `skip` |

`evaluate(policy, question, result)` is a pure function returning
`Verdict::Accept` (the selected candidate or proposition, the evidence, the
`advisory_only` flag) or `Verdict::Abstain` with one of: `low_confidence`,
`near_tie`, `unsupported_candidate` (the model picked an option nobody
offered), `truncated`, `unsupported_question`, `missing_answer`,
`shape_not_allowed`, `provider_failed`. `evaluate_error` turns a failed call
into the last of these.

## The Brain transport

`sven_decision::brain::BrainProvider` is the one `DecisionProvider` that
reaches a model over a wire: a Brain server, which serves the model. Sven
depends on no Brain crate; it speaks HTTP and nothing else, and everything
that knows the shape of the JSON is in `crates/decision/src/brain/wire.rs`.
The recorded exchanges the contract tests pin are in
`crates/decision/tests/fixtures/brain/`.

The base URL defaults to `http://127.0.0.1:8788`, where `brain serve
--openai` listens; a trailing `/v1` or `/` is accepted. A key, when the server
was started with one, is sent as `Authorization: Bearer`.

Calls are `POST /v1/run` with `{model, action, params}`:

| call | model | params | loads the model? |
|------|-------|--------|------------------|
| `models` | `brain/decision` | none | no |
| `select` | `brain/decision` | `{"model": "auto"}` | no |
| `decide` | the chosen model id | `state`, `state_format: "text"`, `questions` (a JSON document, as a string), `deadline_ms` | yes, on first use |

```json
{"model": "convaiinnovations/laya", "action": "decide",
 "params": {"state": "The user asks to rename a function.", "state_format": "text",
            "deadline_ms": 500,
            "questions": "[{\"id\":\"tool\",\"type\":\"choice\",\"instruction\":\"Which tool?\",\"options\":[{\"id\":\"edit_file\",\"description\":\"Edit a file in place.\"},{\"id\":\"grep\"}]}]"}}
```

```json
{"outputs": {"model": "convaiinnovations/laya", "latency_ms": 41.2, "max_input_tokens": 512,
             "flags": {"state_truncated": false, "unsupported_question_types": []},
             "answers": [{"id": "tool", "type": "choice", "confidence": 0.91,
                          "ranked": [{"id": "edit_file", "index": 0, "probability": 0.98},
                                     {"id": "grep", "index": 1, "probability": 0.02}],
                          "state": {"tokens": 31, "kept": 31, "truncated": false}}]},
 "blobs": {}}
```

A proposition is a `noul` question on the wire (candidates `true` and
`false`). `models` answers one record per model with `state` (`ready`,
`not-installed`, `incompatible`), `validated` and a `manifest` carrying
`weights_sha256`, `max_input_tokens` and `question_types`; a model that is not
installed carries the provider's own remedy in `hint`.

**Failures.** The error body's `code` decides, the HTTP status otherwise:

| provider | decision error |
|----------|----------------|
| connection refused, DNS, 401/403, 429/503 | `unreachable` |
| `not_installed`, `model_not_found` | `model_not_installed` |
| `failed_to_load`, an `incompatible` model | `failed_to_load` |
| `unsupported_capability`, no `/v1/run` route | `unsupported_capability` |
| `invalid_request` | `invalid_request` |
| `timeout`, 504, no answer within the deadline | `timeout` |
| `backend_failure`, a body that is not a decision, any other status | `invalid_response` |

**Behaviour.**

- The request's deadline bounds the whole decision: resolving the model,
  waiting for a slot and the call.
- At most `max_concurrent_requests` decisions are in flight (default 2); the
  rest wait inside their own deadline.
- `auto` is the provider's choice. The adapter asks `select` once, keeps the
  answer until the model is reported gone, and every result names the model
  that answered. A configured id is looked up in `models`.
- `MinimumQuality::Validated` refuses a model with no recorded validation as
  `unsupported_capability`, before any decision is sent.
- Decisions of a model that declares `weights_sha256` are cached per process,
  keyed by model id, checkpoint and the normalised request (the deadline is not
  part of it), in an LRU of at most `cache_entries` decisions (default 256). A
  model that declares no checkpoint is never cached.
- `readiness` (`select` and `models`) and `models` never load the model.
- `decide_batched(provider, requests)` sends the independent requests that
  share a state as one call (at most 64 questions per call) and gives each
  asker only its own answers.

## When decisions are unavailable

`decision.mode` ([configuration](../05-configuration.md#decision)) decides
what an unavailable model means:

- `off` (or `decision.enabled: false`, the default): nothing is contacted.
- `auto`: a decision that cannot be answered falls back to the behaviour
  that existed before decisions (for `tools search`, plain BM25 ranking),
  never to a new heuristic. The session says so **once**, as a warning that
  every surface already renders (a `ToolEvent::Notice`, forwarded as
  `SessionEvent::Warning`: the `[sven:warn]` line in headless runs, a notice in
  the TUI, a thought notice over ACP), however many decisions fail afterwards.
  `decision.warn_if_unavailable: false` silences it.
- `required`: before the session starts, readiness is checked (a query that
  loads no model) and a missing model is a startup error with the same text.

The message names the state and the remedy:

| state | remedy named |
|-------|--------------|
| provider unreachable | start the provider, correct `decision.brain.url`, or set `decision.enabled: false` |
| model not installed | the provider's own `brain pull <id>` for the id it reported; never an id the provider did not report; otherwise `sven decisions models` |
| model failed to load | check the model on the provider, or name another in `decision.model` |
| model not usable (for example not validated) | `decision.policy.minimum_model_quality: any` to accept an unvalidated model |
| timeout | raise `decision.timeout_ms` (a first decision may be loading the model) |

Nothing is ever downloaded automatically.

`sven doctor decisions [--json]` reports the selected model, readiness, the
question types it supports, its input limit and the budget one decision sends
(the configured cap within that limit), its validation status, a latency
profile from three tiny decisions (the first may include loading the model),
and every decision point with whether it is active and why not. It contacts the
provider even when decisions are off, and exits non-zero when decisions are
unavailable. `sven decisions models [--json]` lists the models the provider
knows without loading any.

## Tool search re-ranking

The first decision point. `tools` `search` ranks the deferred tools with BM25;
with `decision.enabled` and `decision.features.tool_routing` (both on, and a
usable model) `DecisionRanker` lets the model break ties:

1. BM25 ranks every deferred tool that matches. If its best two are not within
   10% of each other, that is the answer and nobody is asked.
2. Otherwise the best 32 matches are the pool. If the pool is larger than one
   request holds (8 options) and spans several categories, the model first
   picks the category and the pool narrows to it.
3. The best 8 of the pool are the shortlist; the model picks the tool that fits.
   Each option carries the tool's summary; the task is the query, cut to fit the
   input budget (`decision.policy.max_decision_input_tokens` within what the
   served model reports). Options that do not fit are dropped weakest first.
4. Each question is asked with the options in two orders (name ascending and
   descending) in **one** batched call. The answer is accepted only if both
   orders pick the same tool and the policy accepts it.
5. An accepted tool moves to the front. Everything else keeps BM25's order, so
   a re-rank never removes a tool, and `list` and `help` are untouched.

An abstention, a provider failure and `policy.mode: shadow` all return BM25's
ranking exactly; shadow mode still asks and traces. The policy of both
questions: a choice of at most 8 options, routine impact, a wrong answer costs
little either way, winner probability at least 0.55 and a lead of at least 0.15,
fall back to the legacy behaviour.

## Tracing

Every decision is accounted for. The session's `DecisionTelemetry` collects,
between two model requests, the inference calls made and the verdict of every
decision point; the turn executor drains it as the next request's ledger record
is built. The cost lands in that record's `decision` field and the verdicts in
the step's `sven_decisions` extra, next to `sven_ledger`
([token ledger](token-ledger.md)). `trace_record` builds the record from a
verdict: acceptance or abstention, the selected option or the reason
(`low_confidence`, `near_tie`, `order_sensitive`, `provider_failed:<class>`, ...),
and a digest of the inputs rather than the inputs themselves.

## Testing with the mock provider

`MockDecisionProvider` answers from a queue: `push_result` scripts an answer,
`push_error` injects a failure of any class, an exhausted script fails as
`unreachable`, and `requests()` returns everything it was asked.

```rust
let mock = MockDecisionProvider::new("mock/model");
mock.push_result(DecisionResult::for_answers(
    "mock/model",
    vec![("tool", QuestionKind::Choice, Answer::choice(&[("shell", 0.9), ("edit_file", 0.1)])?)],
));
mock.push_error(DecisionError::Timeout { deadline: Duration::from_millis(500) });
```
