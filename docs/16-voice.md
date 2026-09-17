# Voice

Sven accepts spoken instructions: hand it an audio file and the transcript
becomes part of the user turn. Transcription runs against a resident
[brain](https://github.com/mkschreder/brain) server over its capability
surface, so the speech model is loaded once and every later clip costs only
the inference.

> **Status.** Audio *input* is wired and working. The hosted-provider voice
> subsystem described in earlier revisions of this page (ElevenLabs TTS,
> Whisper STT, Twilio outbound calls, a `voice` tool with
> `call`/`synthesize`/`transcribe` actions) exists in the tree but is
> **reachable from nothing**: `IntegrationProviders`' `tts`, `stt` and `calls`
> fields are never populated by any caller, so a `tools.voice` config block
> has no effect. It is documented here as absent rather than quietly dropped,
> because a config key that silently does nothing is worse than one that is
> known to be missing.

## Attaching audio

```sh
sven --attach instruction.wav "and check the tests still pass"
```

`--attach` is repeatable and mixes kinds:

```sh
sven --attach screenshot.png --attach instruction.wav
```

A model that accepts audio natively receives it as audio. Every other model
gets a transcript instead, so the turn survives any provider. Force the
transcript with `force_transcribe` on the `attach_file` tool when you want
text regardless.

In headless mode, close stdin unless you are piping into sven:

```sh
sven -H --attach instruction.wav "" < /dev/null
```

A CI runner or tool harness commonly hands a child an idle pipe that nobody
writes to and nobody closes. Sven cannot distinguish that from a slow
producer, so it waits; after two seconds it says so on stderr, but closing
stdin avoids the wait entirely.

## Configuration

```yaml
tools:
  asr:
    model: brain/nemotronasr            # the served manifest id
    bus_address: "unix:path=/run/brain/bus"   # omit to use the session bus
    timeout_secs: 300
```

`model` is the id the server reports from `ListModels`, which is **not** the
name brain's CLI dispatches on. The served surface uses `brain/nemotronasr`;
the command line uses the bare `nemotronasr`. Neither accepts the other's
spelling.

`bus_address` is only needed when the server is not on your session bus --
notably when it was started detached, since a detached process inherits no
session bus.

## The whole thing in one command

`scripts/voice-e2e.sh` does every step below - a private bus, one brain
server on it, a synthesised clip, a generated config - and hands the result
to sven:

```sh
scripts/voice-e2e.sh run                     # speak, serve, ask
scripts/voice-e2e.sh run --text "Run the tests."
scripts/voice-e2e.sh status
scripts/voice-e2e.sh down
```

Nothing is copied between steps: it fixes `$BRAIN_API_KEY` rather than
reading brain's generated one back out, and writes the bus address it just
created straight into `.sven/config.yaml`. The sections below are that
script's steps, for when you want to run them yourself.

Synthesis happens before the server starts, on purpose: `brain qwen3tts
synth` is a second brain process that loads the TTS model itself, so running
it against a live server would put two of them on one GPU. The clip is
cached, so a re-run with the same `--text` skips it.

## Running a server for it

Any brain serving an ASR model works. A model is served only when its weights
variable is set; `brain serve --help` lists them.

```sh
export BRAIN_NEMOTRONASR=~/.local/share/brain/models/nvidia/nemotron-3.5-asr-streaming-0.6b
brain serve --dbus -d
```

Every weights variable names the checkpoint's directory, including
`BRAIN_QWEN_WEIGHTS` for the LLM that answers the transcribed turn.

`-d` returns once every surface is listening, not merely once the process has
started. `brain serve --status` / `--reload` / `--stop` manage it; one server
runs per user, enforced by a lock on the pidfile. `--status` exits 0 when one
is running and 3 when none is, so a script can branch on it directly.

To reach a detached server from an unrelated shell, give it an address that
outlives the launching shell:

```sh
BUS=$(dbus-daemon --session --fork --print-address=1 --print-pid=1)
ADDR=$(printf '%s\n' "$BUS" | sed -n 1p)
brain serve --dbus-address "$ADDR" -d
```

Confirm the model is actually served -- an unset weights variable is
otherwise silent:

```sh
busctl --address="$ADDR" call com.swedishembedded.Brain1 \
  /com/swedishembedded/Brain1 com.swedishembedded.Brain1.Manager ListModels
```

## Producing test audio

With no microphone, synthesise the clip. This is also better than a recording
for regression tests, because it is reproducible:

```sh
M=~/.local/share/brain/models/Qwen/Qwen3-TTS-12Hz-0.6B-Base
brain qwen3tts synth --ckpt "$M" --seed 7 \
  --text "List the files in the current directory." \
  --out /tmp/cmd.wav
```

Check the result is real audio before trusting a transcription: a silent WAV
transcribes to an empty string, which looks like a broken pipeline when it is
a broken recording.

## What to expect

```
[sven:attach] Transcript of /tmp/cmd.wav (2.0s):

List the files in the current directory.
[sven:thinking] ... the user wants me to list the files in the current directory ...
[sven:tool:call] id="call_0" name="find_file" ...
```

Read that transcript. It is the only place the homophone below shows up
before the model acts on it.

Measured on two Tesla P40s: a 2.0s clip transcribes in about 4.5s against a
warm resident model.

## Troubleshooting

**Transcripts are plausible but wrong on technical words.** "Rust" becomes
"rush"; "hello.txt" becomes "HelloTicks". No speech model resolves a homophone
without domain context, which is why a transcript is merged into the turn for
review rather than executed blind. A small model given a mangled filename
tends to answer from the transcript rather than admit it cannot find the file,
so a wrong transcript reads as a confidently wrong answer, not an error.

**It looks stuck after "Starting brain".** A cold start scans the whole model
directory and activates the first checkpoint before any surface binds, which
takes a minute or two. `brain serve -d` echoes the server's log while it
waits, so you see the scan happen; a run that prints nothing at all is a
brain old enough to predate that.

**The server is up but the start never returns.** Readiness is the AND of
every surface asked for, so a requested surface that cannot bind means the
process can never report itself ready. That is a failed start and ends the
process, with the reason in the log - most often a `--dbus-address` pointing
at a bus that is gone, after a `down` that stopped the bus but left the
address behind.

**Everything is correct but enormously slow.** Check the server log for the
adapter it chose. `adapter: llvmpipe (Cpu, Vulkan)` means it fell back to a
software rasteriser: right answers, orders of magnitude too slow, no error
raised anywhere.

**The run fails before the model replies.** Sven's system prompt and tool
schemas are large; a short spoken command can still carry over 20,000 tokens.
On a small context window the request is refused outright, naming the token
count and the budget. Serve a larger context.

**`could not reach brain for transcription (model brain/nemotronasr)`.** The
server is not running, is on a different bus, or does not serve that model.
`ListModels` above answers all three.
