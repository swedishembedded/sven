<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# tool-syntax - does verified experience teach a small model to look before it acts?

**Status: measured baseline, working demonstration pipeline, training blocked
on a sequence-length limit. No before/after number yet.** What each part does
and what stops it is below, with the numbers that were actually observed.

## The question

`config-discovery` asks for three retry attempts on the upload service this
host runs, without changing what any other deployment does. Which deployment
that is cannot be read anywhere - it is chosen per episode and held in the
service's memory - so the task cannot be started without asking.

## What was measured

| | |
|---|---|
| witness (observation-only solver) | solves either twin in **3 tool calls** |
| Qwen3-0.6B, frozen request | **0 / 6 solved**, 0 errored |
| tool calls per episode | 0, 0, 1, 0, 2, 0 |
| with an exploration hint | 0 → 10 tool calls, still 0 solved |

The failure is specific and repeats. The model does not fail at reasoning
about configuration files; it never gets there. It cannot form a valid tool
call:

```
task: unknown buffer handle '3'. Use `task` or `shell` to create a buffer first.   (9 times in one episode)
system: missing required parameter 'action'                                         (6 times across two)
```

and it does not correct itself when the error names the mistake. A hint makes
it engage without making it succeed.

## What works

```bash
sample-learning-tool-syntax audit        # the catalog checks itself: no model, no GPU, no network
sample-learning-tool-syntax baseline     # the before numbers
sample-learning-tool-syntax demonstrate  # verified episodes -> training data
```

`demonstrate` produces **2/2 verified** demonstrations whose every part is
real: the system prompt and tool schemas from a request the agent actually
sent, the observations from sven's own tool executor, the verdict from the
task's own verifier. Only the choice of action is scripted, which is what a
demonstration is - hence `Scripted` provenance, which can never count as the
model improving on its own.

Run `baseline` first: it writes the captured prompt that `demonstrate` needs.

## What blocks the number

**Training at sven's real prompt length exceeds the hardware.** The single
training example is 5646 tokens, and a LoRA step on it asks for a 1945.6 MiB
storage buffer against a 2048 MiB binding cap:

```
wgpu error: Out of Memory
  last allocation requested: storage (1945.6 MiB)
  device limits: max_storage_buffer_binding_size 2048 MiB
```

Freeing VRAM does not help - it is the sequence length driving one tensor to
the device's limit. CPU is not an escape either: two steps did not finish in
914 seconds. And shortening the block is refused, correctly:

```
--block 1024 is shorter than the longest example (5646 tokens);
it would train on a cut-off answer
```

**The honest way out is a narrower tool set.** The prompt is 76% tool schemas
(17073 chars against the system prompt's 5350), and
`ToolRegistry::schemas_for_mode` already makes the offered set depend on the
mode - an agent offered `shell`, `read_file` and `write_file` is an ordinary
sven configuration, not a rigged one, and lands near ~1800 tokens. It also
removes a confound: the model reached for `task`, among the most complex tools
it was offered, on a task needing none of it.

The cost is that **the baseline must be re-measured under the same mode**.
Both arms have to see the same prompt, or the comparison reports what the tool
list did rather than what was learned. The frozen request, the catalog and the
verifier are untouched by that.

## A second limit worth knowing before building on this

Multi-turn trajectory SFT does not work on this checkpoint, for a reason no
dataset producer can fix. Qwen3's template renders an assistant turn
conditionally on whether anything follows it:

```jinja
{%- if loop.last or (not loop.last and reasoning_content) %}
```

so the same message renders differently in isolation than in context, there is
no honest loss-mask boundary, and brain refuses rather than guessing. One
record per decision does not help - earlier decisions are still context in the
later records. The template's own escape is a non-empty `reasoning_content`,
which `generic-messages-v2` has no field for.

What stays trainable is the **first** decision, whose context is the prompt and
the request alone. For this sample that is the decision that matters, because
the measured failure is acting without investigating.

## Pre-declared, before any training runs

- The measured arms use the **frozen request**. Hints are for collection only
  and are recorded in the report.
- An episode that never reaches a verdict is an error, excluded from both the
  numerator and the denominator - never a zero.
- An arm is scored only against a server proved to apply promoted adapters.
- A demonstration that the verifier does not accept produces no training data.
