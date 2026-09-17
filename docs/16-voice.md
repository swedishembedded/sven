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

## Running a server for it

Any brain serving an ASR model works. A model is served only when its weights
variable is set; `brain serve --help` lists them.

```sh
export BRAIN_NEMOTRONASR=~/.local/share/brain/models/nvidia/nemotron-3.5-asr-streaming-0.6b
brain serve --dbus -d
```

`-d` returns once every surface is listening, not merely once the process has
started. `brain serve --status` / `--reload` / `--stop` manage it; one server
runs per user, enforced by a lock on the pidfile.

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
  --out audio=/tmp/cmd.wav
```

Check the result is real audio before trusting a transcription: a silent WAV
transcribes to an empty string, which looks like a broken pipeline when it is
a broken recording.

## What to expect

```
[sven:attach] Transcript of /tmp/cmd.wav (2.0s):
[sven:thinking] ... the user wants me to list the files in the current directory ...
[sven:tool:call] id="call_0" name="find_file" ...
```

Measured on two Tesla P40s: a 2.0s clip transcribes in about 4.5s against a
warm resident model.

## Troubleshooting

**Transcripts are plausible but wrong on technical words.** "Rust" becomes
"rush". No speech model resolves a homophone without domain context, which is
why a transcript is merged into the turn for review rather than executed
blind.

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
