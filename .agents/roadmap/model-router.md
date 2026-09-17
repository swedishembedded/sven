# model-router

**Status: research done, no implementation started. This document is the plan.**

## Goal

Sven becomes a general-purpose agent the user starts in a directory and
controls entirely through natural language — typed or spoken — where the
request may be satisfied by the current language model alone, or by routing
work to any other model brain can serve (image, video, speech, transcription,
music, depth, segmentation, detection, forecasting, 3D reconstruction), or by
chaining several of them together.

Three properties define "done":

1. **Transport does not matter.** A brain on the local session bus and a brain
   on another machine over HTTP are the same thing to sven's router. Remote
   sven is not a weaker sven.
2. **Capability is discovered, not hard-coded.** Sven ships no per-model tool.
   A model brain gains tomorrow is reachable from sven today.
3. **It fits in the context budget.** Capability is added without growing the
   per-request tool payload — measurements below make this the binding
   constraint on every design choice here.

## The binding constraint, measured

Measured on this tree against a `mock` provider in a minimal `chat` session
(`RuntimeContext::empty()`, so no skills/agents/knowledge sections loaded):

| | chars | ≈ tokens |
|---|---|---|
| System prompt | 5,257 | ~1,400 |
| 19 tool schemas | 24,505 | ~6,600 |

≈ **350 tokens per tool**. The heaviest: `task` 2,535 · `context` 2,253 ·
`gdb` 1,950 · `assimilate_fact` 1,896 · `system` 1,552 · `edit_file` 1,533 ·
`grep` 1,468 · `shell` 1,392.

Brain advertises **66 actions across 44 models** (`brain caps --json`). Exposing
them one-tool-per-action would cost roughly **+23,000 tokens** and put sven at
85 tools — far inside the measured degradation band, where published results
put the onset of tool-selection failure at 20–30 tools and clear quality loss
past 40.

This is why the tool surface below is three tools and not sixty-six.

## What brain already provides

`crates/capability` is a self-describing action interface: every model
advertises a `Manifest` of `ActionSpec`s, each carrying typed `ParamSpec`s
(type, required, default, help, min/max/step, enum values) and named binary
`BlobSpec` inputs/outputs with a declared `Media` kind (`image`, `mask`,
`audio`, `video`, `text`, `bytes`). `ActionSpec::validate` type-checks a call,
fills defaults, and rejects unknown params. `ActionSpec::for_serving` drops
params that name host-only state so a remote caller is never asked for a
filesystem path.

Over D-Bus (`com.swedishembedded.Brain1.Manager`) this is exposed as:

- `Manifests() -> String` — every manifest as JSON (discovery)
- `Run(model, action, params, in_fds, in_meta, transport) -> (result, out_fds, out_meta)`
- `Subscribe(model, action, params, ...) -> (job, event_fd)` — a `SOCK_SEQPACKET`
  fd delivering one framed datagram per event, blobs arriving as `SCM_RIGHTS`
  ancillary fds
- `Cancel(job) -> bool`

Nothing needs inventing here. The work is *reaching* it, remotely as well as
locally, and spending almost no context to do so.

### The 66 actions, by media signature

24 distinct signatures. The long tail is the point: it is what no standard
API dialect can express.

```
11  image -> image      6  - -> bytes       6  image -> text     5  - -> image
 5  - -> text           5  bytes -> bytes   5  - -> audio        3  image -> bytes
 3  audio -> text       2  - -> video       2  bytes -> image    1  image,mask -> image
 1  - -> audio,video    1  bytes -> text    1  text -> text      1  text -> bytes
 1  image,video -> text 1  image -> mask    1  image -> scalar   1  image -> image,mask
 1  audio -> audio      1  bytes,video -> bytes                  1  video -> bytes,video
 1  - -> mask
```

## Why not simply adopt an existing standard

Researched, with the conclusion that the standards are a *projection* of
brain's model rather than a replacement for it.

**Replicate** is structurally the closest: each model version carries an
OpenAPI Schema describing its inputs and outputs, and a generic predictions
endpoint executes any of them. This is brain's `ActionSpec` with less type
information — no declared media kinds, no host-param hiding. It validates the
shape; it does not supersede it.

**OpenAI / OpenRouter / LiteLLM** converged on a *closed vocabulary* of verb
endpoints: `/chat/completions`, `/embeddings`, `/images/generations`,
`/images/edits`, `/audio/speech`, `/audio/transcriptions`, `/videos`,
`/rerank`, `/moderations`, `/ocr`. That covers roughly 8 of brain's 24 media
signatures. Depth, segmentation, detection, time-series forecasting, 3D splat
render/fit, world reconstruction and `lora_train` have no standard endpoint at
all. Adopting the closed vocabulary as sven's interface would strand most of
brain's models.

**One piece is worth adopting verbatim.** OpenRouter's live `/api/v1/models`
(444 models) describes each with:

```json
"architecture": { "modality": "text+image->text",
                  "input_modalities": ["text","image"],
                  "output_modalities": ["text"] }
```

plus `supported_parameters` and `supported_voices`. That `"text+image->text"`
notation is *identical* to the media signature derivable from a brain
`ActionSpec`'s blob lists. Brain should emit exactly these field names, so
discovery vocabulary is shared rather than invented — and a client that
already understands OpenRouter understands brain.

Brain's `/models` today emits none of it; its serving classifier (`CapSet`) is
three booleans — `chat`, `embeddings`, `image`.

## Design

### The router vocabulary (sven, kernel tier)

A second trait beside `ModelProvider`, in `sven-model`, mirroring brain's wire
JSON as plain serde types. **No dependency on any brain crate** — the contract
is the JSON `Manifests()` already returns.

```rust
pub trait ActionProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn manifests(&self) -> Result<Vec<ModelManifest>>;
    async fn run(&self, model: &str, action: &str, inv: ActionInvocation)
        -> Result<ActionOutcome>;
    async fn subscribe(&self, model: &str, action: &str, inv: ActionInvocation)
        -> Result<ActionEventStream>;
    async fn cancel(&self, job: JobId) -> Result<bool>;
}
```

Two implementations, chosen by config, indistinguishable to every caller:
D-Bus (local, zero-copy `memfd` blobs) and HTTP (remote).

`ActionSpec -> JSON Schema` is mechanical, and **must drop `host_env` /
`host_resolved` params client-side**. `for_serving()` is applied by individual
models today, not centrally by `Manifests()`, so a served manifest can still
carry them — `brain/qwen3tts`'s `synth` action advertises `weights_dir` and
`ckpt` as *required*, both env-backed. A language model can never answer those,
and dropping a param must drop it from `required` too.

### The tool surface (sven) — three tools, schemas on demand

- **`models`** — discovery. Filters by modality signature, returns a compact
  line per model (`id`, `text->image`, one-line summary). Cheap enough to call
  mid-turn.
- **`model_schema`** — the full JSON Schema for one `(model, action)`, fetched
  only once the agent has chosen. This is the progressive-disclosure hinge.
- **`run_model`** — executes. `model` accepts a **role alias** or a concrete
  id; `action` is optional and defaults to the model's primary action; brain's
  own `validate` fills every other default. Artifacts are written to disk and
  returned as paths, with progress streamed from `Subscribe`.

The common case stays one round trip: `run_model(model="image",
params={prompt:"a dog"})`. Naming a specific model is the same call with
`model="brain/flux2-klein"`. No semantic per-modality tool is needed, and none
is added.

Roles live in config (`models.image`, `models.voice`, `models.transcribe`,
`models.fast`, …) and resolve through the existing `ModelResolver`, so the same
alias works for a text sub-agent and for an image generation.

### Brain's remote surface

A generic substrate carrying all 66 actions, plus a thin standard-verb
projection for interoperability:

- `GET /v1/capabilities` — manifests, `for_serving()` applied, OpenAPI-described
- `POST /v1/run/{model}/{action}` — typed blobs in/out; `?stream=true` for SSE
  progress mirroring `Subscribe`'s frames
- `DELETE /v1/run/{job}` — cancellation, mirroring `Cancel`
- `/v1/models` gains `architecture.modality`, `input_modalities`,
  `output_modalities`, `supported_voices`
- `/v1/audio/speech`, `/v1/audio/transcriptions`, `/v1/videos`,
  `/v1/images/edits` — the standard verbs, implemented as projections of the
  substrate so the two cannot drift

### Tool composition (sven)

Tool exposure is decided statically by the kernel, not searched for at
runtime by the model. The reason is measurable: prompt caching is a pure
prefix match and the render order is `tools` -> `system` -> `messages`, so
mutating the tool array mid-session invalidates the system prompt *and* the
entire conversation history behind it. A varying tool set is a documented
silent cache invalidator. A sub-agent, by contrast, starts its own short
prefix and pays nothing.

That makes the **agent**, not the tool, the right unit of disclosure: a
persona line in the parent's prompt is a ~15-token pointer to a ~500-token
toolset.

Resolution is one expression:

```
resolved = (core u U included_bundles) \ denied
```

- `tools: [gdb, web]` - **additive** to core. The common case, and the
  inheritance path: a generic agent plus one domain.
- `only_tools: [gdb, shell]` - **exact**; replaces core. Least privilege.
- `deny_tools: [shell]` - subtractive, applied last.
- `task(mode="agent", bundles=["gdb"])` - the dynamic form: spawn a generic
  agent and add tools, with no pre-declared persona. One optional param on a
  tool already in core; the child is a fresh session, so no cache cost.

Deliberately **not** built: persona-extends-persona. Bundles already compose;
chained inheritance adds diamond-resolution ambiguity for no gain. The
lattice stays flat - bundles are the unit of reuse, personas compose them.

Enforcement is at the kernel permission gate, not by omission from the
prompt, so a tool outside an agent's set is structurally unreachable and the
denial is audited. Claude Code's equivalent frontmatter is advisory and has
open bugs where restricted subagents still run forbidden tools; sven's
`Effect` seam makes that failure mode impossible. Default stays
inherit-everything: a persona with no `tools:` key behaves exactly as today.

**Core bundle:** `read_file`, `write_file`, `edit_file`, `grep`, `find_file`,
`shell`, `process`, `buf_read`, `buf_grep`, `todo`, `task`, `skill`,
`attach_file`. Bundles: `gdb`, `web`, `knowledge` (5.3k chars alone),
`context`, `system`, `media`.

**The subprocess gap.** `shell` today takes only `shell_command`,
`timeout_secs` and `workdir` - it is synchronous, with no background spawn,
handle, polling or completion signal. A general-purpose agent must be able to
start work and return to it. The kernel is already event-driven, so a
finished process should post a `SessionEvent` that wakes the agent rather
than the agent spending turns polling. Most machinery exists:
`OutputBufferStore` + `buf_read`/`buf_grep` already hold sub-agent output and
can hold process stdout unchanged, letting the agent grep a large build log
without pulling it into context. The new surface is therefore one `process`
tool with actions (sven's existing idiom - `task` and `context` both do this)
plus the exit event, not four tools.

**What is already built and disconnected.** `ToolSetProfile::detect` already
implements profile selection, including a `has_gdb_config` heuristic keyed on
`.gdbinit`/`openocd.cfg`/`debugging/` - and is called only from its own unit
tests. `RuntimeBuilder::build` hard-codes `ToolSetProfile::Full`, which sets
`include_gdb_context: true`, so every session on every surface pays ~4,200
chars for `gdb` + `context` regardless of project. `Coding`, `Research` and
`SubAgent` are constructed by nothing. Persona frontmatter parses `name`,
`description`, `model`, `readonly`, `is_background` and `knowledge`, but has
no `tools` field.

## Phases

Ordered so the user-visible north star is reachable early and the two large
infrastructure items follow it.

### Phase 1 — the router, locally (sven)

`ActionProvider` and the manifest vocabulary; the D-Bus implementation
extended from today's hard-wired `action: "generate"` to arbitrary
`(model, action)`; `Subscribe` consumed for real (`SOCK_SEQPACKET` frame
reader with `SCM_RIGHTS` ancillary fds) so a multi-minute generation reports
progress and can be cancelled — sven's D-Bus module docs previously declined
this on the grounds that its one model emitted no per-token deltas; image and
video generation make it load-bearing.

*Verification:* wire-contract tests against a fake `Brain1.Manager` over a
`UnixStream` pair, following `crates/model/tests/dbus_provider_test.rs`; then a
real `brain serve --dbus` on this machine generating an image with
`flux2-klein` (Q8_0, 4 GiB, fits one P40).

### Phase 2 — compact tools, roles, sub-agent routing (sven)

The three tools. Model roles in `sven-config`. Two bugs fixed on the way:

- `TaskTool` silently drops a persona's `model:`. `AgentInfo.model` exists and
  the slash-command path honours it (`crates/commands/src/skill.rs:200`), but
  `resolve_mode_and_prompt` returns only `(mode, prompt)`, so an LLM-spawned
  sub-agent always inherits the parent's model.
- No role alias table, so a persona declaring `model: fast` resolves to nothing
  unless `fast` happens to be a provider key.

*Verification:* "generate an image of a dog" and "…using Z-Image" both succeed
end to end against real weights; a sub-agent persona pinned to a different
model demonstrably runs on it; tool payload growth stays within one tool's
worth of the measured baseline.

### Phase 3 — voice, end to end (sven)

Microphone capture in the TUI — net-new, there is no `cpal`/`rodio`/`hound`/
`symphonia` dependency anywhere in the tree today — push-to-talk, transcription
routed through the router to `qwen3asr`/`nemotronasr`, and the transcript
landing **editable in the input box** rather than auto-submitted, so the user
corrects before sending.

The existing `crates/integrations/src/voice/` subsystem is folded in here. It
is currently dead code: `TtsProvider`, `SttProvider`, `VoiceCallProvider`,
`VoiceTool`, ElevenLabs, Whisper-STT and Twilio all compile, but
`IntegrationProviders`' `tts`/`stt`/`calls` fields are never populated by any
caller. The parallel abstraction is removed; speech becomes roles on the
router; the ElevenLabs/OpenAI implementations are retained as alternative
non-brain backends behind the same interface.

*Verification:* speaking into a running sven produces correct editable text in
the input box; speech synthesis round-trips through a configured role.

### Phase 4 — brain's generic HTTP substrate + verb projection (brain)

The endpoints above, with modality metadata on `/models` using OpenRouter's
field names. Standard verbs implemented as projections of the substrate.

*Verification:* the capability list served over HTTP matches the one served
over D-Bus action-for-action; a stock OpenAI-dialect client can drive
`/v1/audio/speech` against brain.

### Phase 5 — transport interchangeability (sven)

The HTTP `ActionProvider`. Phase 1 and 2's test suites are re-run unchanged
against both transports; a remote brain is proven to reach every action a local
one does.

### Phase 6 — tool bundles (sven)

The core bundle above becomes the default, everything else becomes an opt-in
bundle, and `ToolSetProfile` (dead today) is replaced by the composition
model. This is where the 24,505-char baseline comes down.

Also closes the subprocess gap, which is a prerequisite rather than a nicety:
today `shell` is synchronous-only, so the agent cannot start long work and
return to it.

*Risk to respect:* the bats suite in `tests/e2e/basic/` pins headless output
tokens as a public contract, and existing prompts reference tool names
directly. Both must survive.

## Open items

- No TTS checkpoint is pulled locally (`brain models list --local` shows
  qwen35, qwen3vl, fastvlm, qwen3asr, nemotronasr, sam2, s3dit, flux2, wan).
  Speech *synthesis* can be built and wire-tested but not run end to end until
  one is fetched; transcription can be fully verified.
- Whether brain's HTTP substrate should require auth per-dialect as the
  existing surfaces do, or carry one key for the generic surface.
- Whether `ground` (`crates/tools-ground`, currently shelling out to the `brain`
  CLI per call) folds onto `ActionProvider` once it exists. It should; it is the
  same call through a worse transport.
