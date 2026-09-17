# voice-e2e-runbook

How to run the voice-to-agent path end to end: generate a spoken command,
feed it to sven, and have the agent act on it. Every step below was run on
this hardware; the numbers are measured, not estimated.

There is no microphone here, so speech is **synthesised** rather than
recorded. That is not a workaround for testing's sake: it makes the input
reproducible, which a microphone never is.

## What talks to what

```
qwen3tts  ──WAV──>  sven --attach  ──f32 PCM over D-Bus──>  nemotronasr
                          │                                      │
                          │<───────────── transcript ────────────┘
                          v
                    user turn  ──HTTP──>  Qwen3-0.6B  ──> tool calls
```

Both models are served by **one resident brain process**. sven never spawns a
subprocess: transcription is a `Run` of the `transcribe` action over the
capability surface, so the 2.4 GiB ASR checkpoint is loaded once rather than
per clip.

## Prerequisites

Checkpoints (`brain pull <ref>`; `brain models list --local` shows what is
present):

| Arch | Repo | Role |
|---|---|---|
| `qwen3tts` | `Qwen/Qwen3-TTS-12Hz-0.6B-Base` | synthesises the command |
| `nemotronasr` | `nvidia/nemotron-3.5-asr-streaming-0.6b` | transcribes it |
| `qwen3` | any served chat checkpoint | answers it |

## 1. A bus the server can keep

A detached server inherits no session bus, because the shell that launched it
is gone. Give it an address that outlives that shell:

```sh
BUS=$(dbus-daemon --session --fork --print-address=1 --print-pid=1)
ADDR=$(printf '%s\n' "$BUS" | sed -n 1p)   # line 1 is the address
BUSPID=$(printf '%s\n' "$BUS" | sed -n 2p) # line 2 is the pid
```

## 2. One brain, serving both surfaces

D-Bus carries transcription; HTTP carries chat. A model is served only if its
weights variable is set -- `brain serve --help` lists them.

```sh
export BRAIN_NEMOTRONASR="$HOME/.local/share/brain/models/nvidia/nemotron-3.5-asr-streaming-0.6b"

brain serve \
  --dbus-address "$ADDR" \
  --openai 8788 --api-keys-out /tmp/brain_keys.json \
  -d
```

`-d` returns only once every requested surface is listening, so the next
command can assume the server is up. It prints the pid and the log path.

```sh
brain serve --status     # running, and its pid
brain serve --reload     # replace it after a rebuild or an env change
brain serve --stop
```

Exactly one server runs per user, enforced by a lock on the pidfile. Starting
a second one is refused, naming the pid holding it.

Confirm the ASR model is actually being served -- a missing weights variable
is silent otherwise:

```sh
busctl --address="$ADDR" call com.swedishembedded.Brain1 \
  /com/swedishembedded/Brain1 com.swedishembedded.Brain1.Manager ListModels
```

## 3. Synthesise a command

```sh
M="$HOME/.local/share/brain/models/Qwen/Qwen3-TTS-12Hz-0.6B-Base"
brain qwen3tts synth --ckpt "$M" --seed 7 \
  --text "List the files in the current directory." \
  --out audio=/tmp/voice/cmd.wav
```

A 2.0s clip at 24 kHz. `--seed` makes it reproducible. Check it is real audio
rather than silence before trusting a transcription result: a silent WAV
transcribes to an empty string, which reads like a broken pipeline when it is
really a broken recording.

## 4. Point sven at the server

`.sven/config.yaml` in the working directory:

```yaml
model:
  provider: openai
  name: Qwen/Qwen3-0.6B
  base_url: http://127.0.0.1:8788/v1
  api_key: "<from /tmp/brain_keys.json>"
  max_tokens: 40960
tools:
  asr:
    model: brain/nemotronasr     # the SERVED manifest id, not the CLI arch id
    bus_address: "unix:path=/tmp/dbus-XXXX,guid=..."
    timeout_secs: 300
```

Two ids are easy to confuse and neither surface accepts the other's spelling:
`ListModels` reports `brain/nemotronasr`, while `brain <arch> <action>` on the
command line takes the bare `nemotronasr`.

## 5. Run it

```sh
sven -H --attach /tmp/voice/cmd.wav "" < /dev/null
```

`--attach` is repeatable, and mixes kinds: `--attach shot.png --attach
ask.wav`.

`< /dev/null` matters whenever stdin is redirected but nobody will write to
it -- a CI runner or a tool harness hands a child an idle pipe, and sven
cannot tell that apart from a slow producer, so it waits. It says so on
stderr after two seconds rather than sitting silent, but closing stdin avoids
the wait entirely.

Expected shape:

```
[sven:attach] Transcript of /tmp/voice/cmd.wav (2.0s):
[sven:thinking] ... the user wants me to list the files in the current directory ...
[sven:tool:call] id="call_0" name="find_file" ...
```

## Measured on this hardware

| | |
|---|---|
| Transcription, warm resident model | **4.5 s** for a 2.0s clip |
| Transcription, previous per-call subprocess | 13.5 s |
| Cold `-d` start (scan + bind both surfaces) | ~100 s |
| Repeat `brain serve -d` when already running | refused immediately |

## Things that will bite

**Prompt size.** A two-second command carries ~22,000 tokens of system prompt
and tool schemas. On an 8k-context model the run fails outright with "prompt
(~23,938 tokens) leaves no room for a response". Give the server a context
comfortably above that (`--qwen-ctx 40960`) until the tool surface shrinks.

**Silent CPU fallback.** If the server log says `adapter: llvmpipe` rather
than naming your card, every model is running on a software rasteriser: right
answers, orders of magnitude too slow, and no error anywhere. Check the log
after starting:

```sh
grep -E "adapter:|placement" "$(brain serve --status | sed 's/.*log //;s/)//')"
```

**Homophones.** "Rust" transcribes as "rush". No ASR model fixes this without
domain context, which is the argument for the transcript landing editable in
the input box rather than being submitted straight through.
