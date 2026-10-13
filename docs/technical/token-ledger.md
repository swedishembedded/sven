# Token ledger

Every model request sven makes carries a ledger record: an attribution of the
request's input tokens to their sources, reconciled with what the provider
reported. It exists so that any change meant to save tokens can be shown to
save them, and to show *which* tokens: the system prompt, a particular tool's
schema, a large tool result, replayed history.

The provider's reported prompt size stays authoritative for billing. The
ledger's figures are estimates that say where the tokens came from; the
residual between the two says how far the estimate is off.

## One record per request

`sven_turn::ledger::estimate_request` is a pure function of the
`CompletionRequest` about to be sent. `stream_turn` calls it at the one place a
request is assembled, feeds the record every usage report the provider streams
for that request, and emits it as the last event of the response: a
`SessionEvent::TokenUsage` whose counters are all zero and whose `breakdown`
is set. Counter-only consumers (the TUI meter, ACP turn totals, the
`[sven:tokens]` line) therefore see nothing new; `breakdown` is `None` on
every other `TokenUsage` and absent from events recorded before the ledger.

A record is one JSON object (`sven_vocab::RequestBreakdown`). Empty maps and
absent optionals are omitted; every field has a default, so old and new
records read each other.

| field | meaning |
|-------|---------|
| `estimator` | name of the estimator that produced the figures (`bytes_div_4`) |
| `system_stable` | system instructions that do not change within a session |
| `system_runtime` | system-prompt sections built from the runtime context, by section: `Skills`, `Subagents`, `Knowledge Base`, `Project Context`, `Project Instructions`, `Git Context`, `CI Environment`, and `dynamic_suffix` (the uncached suffix) |
| `tool_schemas` | each tool's schema (name, description, parameters), by tool name |
| `user` | user-authored text |
| `assistant` | assistant text replayed from history |
| `tool_call_arguments` | tool-call names and arguments replayed from history, by tool name |
| `tool_results` | tool-result text replayed from history, by the tool that produced it (`unknown` when the call is not in the history) |
| `media` | images and audio, wherever attached |
| `estimated_total` | the sum of every figure above |
| `reported` | what the provider reported for the request, summed over its usage reports: `input`, `output`, `cache_read`, `cache_write`; absent when it reported nothing |
| `residual` | `reported` logical prompt minus `estimated_total`; positive means the estimator undercounts; absent exactly when `reported` is |
| `tool_activity` | `calls` and `duplicate_calls` over the tool calls in the history sent with this request; `retries` and `errors` are reserved and absent |
| `decision` | the decision-model inference the request's context was shaped by, since the previous request: `calls` (cache hits are not calls), `latency_ms` (wall time waited), `model_id`, `batch_size` (the most questions one call carried); `load_time_ms` stays empty because the provider does not report it. Absent when no decision ran. See [decisions](decisions.md) |
| `wall_ms` | time from sending the request to the end of its response |

Three notions of prompt size are kept apart. The **logical prompt** is
everything the model read: `input + cache_read + cache_write`, and it is what
`residual` is measured against. **Cached** is the part served from the
provider's cache (`cache_read`). **Billed input** is what is charged at the
uncached and cache-write rates (`input`, `cache_write`); the ledger records
counts and never a price.

A duplicate tool call is one with the same tool name and the same arguments as
an earlier call in the history, compared as JSON with object keys sorted.
Because the history is what a request carries, the counters are snapshots that
grow through a session (and shrink if compaction drops history); a trajectory
summary takes the largest snapshot, not a sum.

## Estimator

No tokenizer for the active provider is available in the workspace, and
providers disagree with each other, so the estimator is deterministic and
provider-independent: text costs 4 bytes per token, rounded up
(`sven_turn::ledger::BYTES_PER_TOKEN`). That is the constant behind
`Message::approx_tokens` and the request-size gate in `sven_model::budget`, so
the ledger and the gate agree on what a token is. Images and audio use the same
approximations as `Message::approx_tokens` (85 tokens for a low-detail image,
765 otherwise; about 50 tokens per second of audio, at least 100). Dense text
(non-Latin scripts, minified code) costs more than 4 bytes per token and shows
up as a positive residual rather than as a hidden correction.

## Where it is stored and read

- **Headless stderr.** `--token-ledger` prints each record as
  `[sven:ledger] {json}` with a 1-based `request` index added. The
  `[sven:tokens]` line is unchanged.
- **Trajectory.** The runner saves each record in the `extra` of the agent step
  it belongs to, under `metrics.extra.sven_ledger` (an array, because a step can
  cover several requests). The provider-reported counters of the same requests
  are summed into the step's `prompt_tokens` (cached tokens included, as ATIF
  defines it), `completion_tokens` and `cached_tokens`. A subagent's records are
  saved the same way in its embedded trajectory.
- **Summary.** `sven ledger FILE` sums a trajectory and every subagent
  trajectory embedded in it (`sven_session_store::step_ledger::summarize`) and
  prints a table, or with `--json` a `sven_vocab::LedgerSummary`. A trajectory
  recorded before the ledger existed is reported as holding none, not as zeros.

The decision traces of a step are saved beside the ledger, under
`metrics.extra.sven_decisions` (an array in the order the decisions happened):
for each acceptance or abstention, the policy label, a SHA-256 of the state and
questions asked (`inputs_digest`), `verdict` (`accept` or `abstain`), what was
`selected` or why it abstained (`abstain_reason`), the `model_id`, whether the
answer was `advisory_only`, and whether it was `applied` (false in shadow
mode). A tool that was promoted, or one that was not, can be explained from the
trajectory alone. A ledger record in the trajectory never contains them.

A saved record is never rewritten: a reader that meets an unreadable record
reports an error rather than skipping it.

## Surfaces

| surface | handling |
|---------|----------|
| headless runners (`RuntimeRunner`, `CiRunner`) | record in the trajectory; `[sven:ledger]` with `--token-ledger`; subagent records from `CiRunner` |
| ACP | sent as a `UsageUpdate` whose `used` is the request's prompt size and whose `_meta.sven_ledger` is the record; a parent session folds a subagent's into the subagent's trajectory |
| TUI | none: the TUI shows context use from the provider's counters and rebuilds its saved trajectory from the chat, which carries no per-request data |
| SDK | the field is on `SessionEvent::TokenUsage`, which `Agent::events` forwards unchanged |

## Baseline

`make benchmark/ledger` replays fixed scenarios headless against the `mock` model
provider and writes, outside the repository, each scenario's trajectory,
`sven ledger` table and JSON, and a `report.json` / `report.txt` that names the
exact sven commit (and whether the working tree had uncommitted changes).
`OUT=DIR` chooses the directory; the default is `$TMPDIR/sven-ledger-bench`,
and an existing directory is only replaced if it is a previous result.
`benchmarks/ledger/run.sh --bin PATH` measures a binary other than the one
`make build` produces.

The scenarios run with an empty home directory, so that skills, agents,
knowledge and configuration of whoever runs them do not enter the request.
A request quotes the working directory in its project-context section, so
compare runs made with the same `--out`.

| scenario | what it exercises |
|----------|-------------------|
| `minimal` | one prompt with the default tool set: the fixed cost of a request |
| `tool_heavy` | four user turns that fan out into shell, file reads, a search and a write, one of them re-reading a file |
| `noisy_output` | one shell command that prints 3,000 log lines |
| `long_history` | a 60-turn conversation replayed ahead of one more question |
| `deferred_tool` | one call to a tool outside the core set; under deferred disclosure it is loaded first |

Estimated input tokens, summed over the requests of each scenario (`bytes_div_4`;
the mock provider's own usage numbers are synthetic, so the baseline does not
compare against them):

| scenario | requests | system | tool schemas | user + assistant | tool calls + results | total |
|----------|---------:|-------:|-------------:|-----------------:|---------------------:|------:|
| `minimal` | 1 | 1,370 | 4,253 | 5 | 0 | 5,628 |
| `tool_heavy` | 7 | 9,597 | 29,771 | 268 | 45,668 | 85,304 |
| `noisy_output` | 2 | 2,742 | 8,506 | 10 | 2,689 | 13,947 |
| `long_history` | 1 | 1,371 | 4,253 | 3,307 | 0 | 8,931 |

In `tool_heavy` the replayed tool results are 53% of everything sent and the
schemas 35%: each request pays again for every earlier result and for all
fifteen schemas. In `noisy_output`, 3,000 lines reach the model as about 2,600
tokens because the shell tool elides the middle of a long output.

### What a request costs before the user says anything

Measured by the `minimal` scenario in each mode, in estimated tokens
(`bytes_div_4`):

| mode | system (stable) | project context | tool schemas | tools | total |
|------|----------------:|----------------:|-------------:|------:|------:|
| `agent` | 1,291 | 79 | 4,253 | 15 | 5,628 |
| `plan` | 1,334 | 79 | 4,253 | 15 | 5,671 |
| `chat` | 1,190 | 79 | 4,253 | 15 | 5,527 |
| `research` | 1,217 | 79 | 3,227 | 11 | 4,528 |

### Tokens per tool schema

The default tool set of a headless `agent`, `plan` or `chat` run (a build with
the default features, no MCP servers, no skills discovered), by estimated
tokens of the schema the model is sent (name, description, parameters):

| tool | tokens | tool | tokens |
|------|-------:|------|-------:|
| `task` | 591 | `shell` | 252 |
| `grep` | 399 | `memory` | 247 |
| `system` | 395 | `read_file` | 219 |
| `edit_file` | 385 | `attach_file` | 196 |
| `semantic_memory` | 342 | `write_file` | 193 |
| `todo` | 324 | `web_fetch` | 140 |
| `find_file` | 296 | `skill` | 137 |
| | | `web_search` | 137 |

That is 4,253 tokens for 15 tools, 284 on average, with the largest schema
(`task`) four times the smallest. `research` mode offers 11 of them.

Two of these figures grow with what is installed rather than with sven. The
`skill` tool's description lists every discovered skill, and the `Skills`
system section lists them again, so skill metadata is paid for twice on every
request. MCP tools add their own schemas, charged to their own names.

These replace the earlier planning figures of about 350 tokens per tool and
about 1,400 system tokens, which were taken on a 19-tool chat session; the
ledger measures what the headless runner actually sends.

## Deferred tool disclosure

`--tool-disclosure deferred` (the default for a program that reads the
configuration file) sends the definitions of the core tools only. In the ledger
the other tools' `schema:` lines disappear, the `tools` tool appears, and the
system prompt gains a `Deferred Tools` section (counted in `system_runtime`).
`make benchmark/ledger DISCLOSURE=full|deferred` measures either mode, and the
measured savings are in
[tool-disclosure.md](tool-disclosure.md#measurements).
