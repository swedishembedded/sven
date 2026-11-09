<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

Swedish Embedded AB implements solutions for agents that combine image,
speech, vision, music and video models for its clients. If your team needs
expertise in multimodal agent orchestration then you can procure our services
by sending an email to info@swedishembedded.com.
-->

# Media artifacts and model invocations

How an agent runs models of any modality and passes what they make from one
to the next (to another model, to a sub-agent, to the person) without
copying bytes through its own language model. Status and open work: the
[model-router ledger](../../.agents/roadmap/model-router.md), Phase 2m.

## Means, not policy

sven provides mechanisms; which model to use, in what order, in parallel or
through sub-agents, is decided by the agent at run time.

| mechanism | what it gives the agent |
|---|---|
| `models` | the models it may run, filtered by input and output modality or text, one line each |
| `model_schema` | one model's operations, parameters (enums, ranges), voices and prices |
| `run_model` | one invocation: `model`, `operation`, `prompt`, `inputs` (handles or paths), `params`; answers text and handles |
| `attach_file` | puts one artifact (a path or a handle) in front of the session's own model |
| `task` `inputs` | hands artifacts to a sub-agent by handle |

## Handles

An artifact is named by a handle and never by its bytes:

- `mcp-file://files/<id>`: an artifact the sven service keeps for the
  conversation (signed in, in `sven rc` or hosted). Its bytes stay there;
  the service resolves the handle whenever a model reads it.
- a working-directory path: an artifact on this machine (a terminal sven
  without a service conversation, or a bring-your-own provider).

A handle is opaque to the language model: it copies it from one tool result
into the next call. `ArtifactRef` (`uri`, `name`, `mimeType`, `size`) is the
vocabulary type; it rides `ToolOutput.artifacts` and the session event
`ToolCallFinished.artifacts`, which is how a client (the TUI, a browser
through `sven rc`) shows what a tool made. A tool that publishes artifacts
does not also send inline image copies of them.

## What the session's model sees

Showing an artifact to the person is not showing it to the model. A
generated image is put in front of the session's model when that model takes
images (so "make it darker" refers to what it can see); audio and video are
not, since a model rarely benefits and pays heavily. To look at anything
else, the agent calls `attach_file` with its handle, or runs a model that
reads it (`run_model` with `operation: chat` and the handle as input), which
answers text and spends none of the session's context on bytes.

## One invocation

`run_model` sends one request to the service's invocation API with an
idempotency key derived from the session and the tool call (a retried call
after a restart answers the same result and is charged once) and, inside a
service conversation, the conversation's id, so outputs belong to it. A long
operation (video) is polled inside the call. Without a conversation the
outputs are downloaded into `tools.media.dir` and answered as paths.

## Sub-agents

`task` takes `inputs`: the child's first prompt carries each image it can see
as an image and every input by handle, so it can pass them on. The child
runs with the parent's key and conversation (never a wider one), so what it
makes is published like the parent's, and the parent's result lists the
handles the child made. For a one-shot question to another model (three
vision models labelling one picture), `run_model` with `operation: chat` is
the cheaper path; `task` is for multi-step work.

## History

A conversation restored from a server (`HistoryEntry`) keeps each message's
handles (`attachments`) and renders them as a short reference list. Nothing
is fetched until the agent asks for it.
