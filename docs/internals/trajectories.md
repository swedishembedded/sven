# Training-grade trajectories

What a run's ATIF trajectory records so that an application that trains models can
learn from it without re-deriving anything, and so that a run can be checked and,
where the stack allows, reproduced. sven records; applications consume. Nothing here
makes an agent depend on any trainer.

## What a step carries

An agent step is built from the conversation record, never from the compacted view
the model reads (see [Compaction](#compaction-is-marked-never-hidden)). For each model
request a step covers:

| Where | What | Source |
|---|---|---|
| `model_name` | the model identity the server says answered | the response's `model` |
| `metrics.prompt_token_ids` | the prompt as the model read it | server echo (`prompt_token_ids`) |
| `metrics.completion_token_ids` | the sampled tokens, in order | server echo (`token_ids`) |
| `metrics.logprobs` | the log-probability of each sampled token under the distribution it was decoded from | server echo (`logprobs.content[].logprob`) |
| `metrics.extra.sven.seed` | the seed the server reports it used (the requested one when it reports none) | the `seed` of the response's `brain` object, else the request |
| `metrics.extra.sven.adapter` | `{id, digest}` of the adapter that answered | `adapter` of the `brain` object |
| `metrics.extra.sven.base_digest` | `sha256:` of the base checkpoint | `base_digest` of the `brain` object |
| `metrics.extra.sven.backend`, `engine` | the compute backend and engine build that decoded | `backend`, `engine` of the `brain` object |
| `metrics.extra.sven.requests` | each request's own prompt ids, sampled ids, log-probabilities, seed and token counts, when the step covers more than one request | the same |

The `brain` object is a non-standard member of the streamed response frames that a
[brain](https://swedishembedded.com/docs/brain) server adds; `prompt_token_ids`, `token_ids` and
`logprobs` are read from any OpenAI-compatible server that echoes them.

A step covers more than one request when a tool call and the answer after its result
share a step: the step's own `prompt_token_ids` are then the first request's and the
sampled ids of all of them run together, so `extra.sven.requests` is the authority for
per-request training data. Every field is absent when the server does not report it;
none is guessed. Sampling that was asked for goes on every request: `RunOptions::sampling`
(temperature, top-p, top-k, seed) and `RunOptions::want_logprobs` for the SDK, the same
for every child the run starts.

The log-probability is the sampler's: the temperature-scaled, top-k and top-p truncated
softmax a token was drawn from, and `0.0` under greedy decoding. It is not the untruncated
model distribution; a consumer that needs that scores the tokens with a forward pass.

## Capture, evaluation, reward and the training record are four things

- **Capture** is the trajectory: what was said and done, and how it was decoded.
- **Evaluation** is a verifier's verdict on the work. It is produced outside the agent and
  is not in the capture.
- **Reward** is derived from a verdict. `SessionOutcome` returns `Unknown` for a claimed
  success that no verifier backs, and a trajectory without `final_metrics.extra.reward`
  means the outcome is unknown, never "good".
- **The training record** is a consumer's projection of capture plus verdict. An application
  decides what it admits; a trajectory that marks itself incomplete or compacted is not
  something a projection of whole conversations can use.

## Compaction is marked, never hidden

A context compaction shrinks what the model reads next, not what happened. The agent keeps
an append-only record of every message beside the history the model reads, and the export is
built from the record: every step stays, and the compaction is a `System` step with
`extra.context_management` where it fired. A child run's store keeps the same transcript. A
consumer that trains on whole conversations excludes a trajectory that holds a compaction
marker, because what the agent saw after it is not the steps before it.

## Crash-robust capture: the journal

`Agent::journal_to(path, policy, limits)` appends the trajectory to a new file as it is
produced, one record per line ([`atif::persist`](../../crates/atif/src/persist/journal.rs)):
a header, one record per settled step, and an end record written only by
`Agent::close_journal`. A step is written once it can no longer grow; a run parked on a
question holds its last step back. The writer hands each record to the kernel in one write,
so a killed process leaves every record that finished, and the `FlushPolicy` (every record,
every n records, or on close) says how much a power loss may take. The record size and file
size are bounded; a bound that is hit, or a write that fails, stops the journal, raises a
warning and is reported by `journal_error` and `close_journal`; it never fails the run.

`read_journal` returns the trajectory with `extra.capture_status` set to `complete` or
`incomplete`, tolerates a record cut short at the end (counted in `torn_tail_bytes`) and
refuses damage before the last record. A journal without its end record was never finished
and its trajectory says so; a consumer that admits only finished runs refuses it by reading
that field. The kill test (`crates/sdk/tests/journal_kill.rs`) runs a real second process,
SIGKILLs it inside a model request and reads the journal back.

## Children

A harness run's parent trajectory embeds every finished child as a trajectory in
`subagent_trajectories` (each with a `trajectory_id`) and lists them in
`extra.harness.children`. A child's trajectory is built from the store it ran in:
every message, the tool calls with their results, each request's account including its
seed and token ids, and its compactions. Calls the permission gate refused are answered in
the transcript like any other. Nested children are listed flat, each with its descriptor
(harness, entry, depth).

## Reproducibility

- With a scripted mock provider a run is exactly reproducible: the same scripts, tools and
  sampling give the same event stream and the same trajectory (`crates/sdk/tests/reproduce.rs`),
  and a trajectory alone holds what is needed to rebuild the model's side of the stream.
- The seed recorded is the one the server reports it used, also for a request that
  carried none (the server draws one and echoes it); only a server that reports none
  leaves the step with the requested seed or none.
- Whether a decode repeats for the same seed is a property of the server, not of sven.
  The trajectory therefore records what lets a difference be explained rather than
  prevented: the seed, the sampled token ids and their log-probabilities, the answering
  model, the base checkpoint and adapter digests, the backend and the engine build, each
  when the server reports it.
