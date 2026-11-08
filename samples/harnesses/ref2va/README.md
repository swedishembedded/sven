<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# ref2va - a clip rendered from a character image, with no model turn

A harness of three check tasks: the inputs are checked, a video model renders
a clip from a reference image, and the clip is checked exactly. No task has a
prompt and `agent.profile` is `none`, so no chat model is ever asked: a machine
whose accelerators the video model needs to itself runs it without one
resident beside it, and `sven harness run` takes no `--model`.

What it shows:

- **A render is a trusted command.** The `render` gate runs `sven media
  generate`, which makes exactly the request the `run_model` tool makes
  (reference, `ref2va` action, exact size, seed, the action's parameters) and
  writes the clip. The gate's timeout is 3600 s, the longest a gate may have.
- **Running it again never pays twice.** The request is named by what it asks,
  the content of the reference included. Run the same command again and the
  clip already written answers; if the run was killed while the server was
  still rendering, the rerun asks under the same name and reattaches to the job
  the server still has (a server that restarted forgot its jobs and renders
  again). A different seed, prompt, size, step count, preset or image is
  another request.
- **The result is checked exactly, not trusted.** `sven media probe` refuses a
  clip that is not a whole MP4, whose picture is not the width and height
  asked for, that does not hold exactly `frames` frames, whose length is not
  `seconds` (to within half a second) or that has no sound.
- **The clip is published.** The last task publishes the output file, so the
  run's trace records its digest.

## What you provide

- **A character image you own, or made yourself**, in the workspace: png, jpg,
  jpeg or webp. The sample ships none, and nothing here refers to any image
  you did not make.
- **The video model's licence.** The brain model `minimaxh3` refuses every
  request unless the request confirms the operator is authorized to use its
  weights. The `licence_confirmed` input is that confirmation: set it to
  `true` only if you hold the model's licence. The run stops before any
  request while it is not `true`.
- **A media provider in sven's configuration.** The `render` gate is a
  separate `sven` process, so it reads the configuration of the user running
  it, not flags of `harness run`. For a brain on the same machine:

  ```yaml
  # ~/.config/sven/config.yaml
  tools:
    media:
      provider: brain
      timeout_secs: 3600   # a render may take longer than the default 30 minutes
  ```

  A running brain is found without further settings; any other
  OpenRouter-compatible video API is a `providers.<name>` entry with its
  `base_url` and a key (`sven auth set <name>`).
- `sven` on the `PATH` (the gates run it).

## Run it

```sh
sven harness validate samples/harnesses/ref2va
sven harness run samples/harnesses/ref2va --workspace DIR --approvals auto \
    --input '{"prompt": "<Picture 1> turns toward the camera and says hello",
              "reference": "character.png", "model": "brain/minimaxh3",
              "width": 768, "height": 448, "frames": 121, "seconds": 5.0417,
              "steps": 4, "seed": 1, "preset": "turbo-4step-vsa",
              "licence_confirmed": true, "output": "result.mp4"}'
```

`DIR` holds `character.png`; the clip is written to `DIR/result.mp4`. Nothing
else is needed: `--model` and `--model-endpoint` are for harnesses that ask a
model. `--approvals auto` lets the gates run (`deny` refuses them, `ask`
puts each to you).

If a gate fails the run stops there, with the reason as the command's last
line on standard error (a refusal from the video model arrives that way).
Fix the cause and run the same command again: the checks that passed pass
again at once and the render reattaches or is answered from the clip already
written. Do not pass a short `--timeout`: it suspends the run after that many
seconds, and a render takes minutes.

The inputs:

| Input | |
|---|---|
| `prompt` | What the clip shows. Names the references by order within their kind: `<Picture 1>` is `reference`. |
| `reference` | Workspace-relative path of the character image. |
| `model` | The video model, e.g. `brain/minimaxh3`. |
| `width`, `height` | The picture in pixels, a size the model renders (for `minimaxh3` a multiple of its stride, e.g. 768 and 448). |
| `frames` | The exact frame count, on the model's frame grid: the model snaps any other count up, and the probe then refuses the clip. |
| `seconds` | The length those frames make at the model's rate (121 frames at 24 fps is 5.0417 s). It is checked, not requested: the request sets the length by `frames`, and the model refuses one that sets it twice. |
| `steps`, `seed` | Denoising steps; another seed is another take. |
| `preset` | What the model runs with, by the server's own name: a request never names a file on the server. For `brain/minimaxh3`, `base` (the release alone), `turbo-4step` (a 4-step distillation adapter; set `steps` to 4) or `turbo-4step-vsa` (that adapter with sparse attention and its trained gates). The server must hold the files the preset names. |
| `licence_confirmed` | `true`, as above. |
| `output` | Workspace-relative path of the clip to write. |

## What each gate guarantees

| Gate | Guarantee |
|---|---|
| `inputs` | The licence is confirmed, the reference is a non-empty png, jpg, jpeg or webp file inside the workspace, and the output is a path inside it. Nothing has been sent to any server. It checks the name and size of the file, not that its bytes decode as an image; the server's refusal of one that does not is reported by `render`. |
| `render` | `sven media generate` exited 0: the provider delivered a clip and it is written at `output`. |
| `clip` | The file is a whole MP4 of exactly `width` x `height` pixels and `frames` frames, as long as `seconds` says, with a sound track. |
