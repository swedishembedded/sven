<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# short-film - a film planned, rendered shot by shot and joined

A workflow with three tasks: a director plans the shots, each shot is rendered
by a video model and checked, then the shots are joined into `film.mp4`.

What it shows:

- **The shot list is a typed variable.** The planner sets `shots` (an array of
  `{id, prompt}`, checked against its schema); the render loop freezes it when
  it begins, so no later turn can add, drop or reorder a shot.
- **Every shot is checked by a trusted command before the next begins.**
  `sven media probe --seconds N` refuses a clip that is not a whole MP4 or is
  not the length asked for (a cut-short download, an error page saved as a
  clip); the agent is then asked to render it again with another seed.
- **Progress survives a restart, and nothing is paid for twice.** With
  `--checkpoint-out`, each finished shot is a checkpoint; `run_model` names
  each request by what it asks, so a resumed or repeated run answers the clip
  already made instead of rendering it again.
- **The film is joined by the executor, not by the model.** The `film` check
  runs `sven media join` on the planned list, in order: the clips are copied as
  they are, sound kept in sync with each shot's picture.

All shots must come from one model at one size: joining copies the clips
without re-encoding, so they must be encoded alike.

## Run it

```sh
sven harness validate samples/harnesses/short-film
sven harness run samples/harnesses/short-film --workspace DIR \
    --input '{"premise": "a paper boat in a storm", "model": "<video model>", "shots": 6, "seconds": 5}' \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --media-endpoint https://<video API>/v1 --media-key-file KEYFILE \
    --approvals auto --checkpoint-out film.checkpoint.json
```

`DIR` is an empty directory the film is made in; `--media-endpoint` is any
OpenRouter-compatible video API (a local `brain serve`, OpenRouter, the sven
service) and `KEYFILE` holds its key, outside `DIR`. The checks run `sven`, so
it must be on the `PATH`. `run_model` reports each shot's price; the run spends
money for every shot it renders.
