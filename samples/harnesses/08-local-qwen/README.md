<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# 08-local-qwen - a real local model (opt-in)

The one sample that names a real model: a schema-constrained sentiment label
from `ollama/qwen3:8b`. It exists so a harness can be tried against a model
you actually run - and it is **never** part of any default path.

## The rules

- **Opt-in only.** `cases/smoke.yaml` names a `live` model. Without the
  opt-in the case is reported *skipped*, never failed, and nothing is
  contacted.
- **You serve the model.** The caller must already be running it. Sven never
  pulls, downloads, starts or loads a model for this sample.
- **Both halves are needed**: `SVEN_HARNESS_LOCAL_MODEL=1` and
  `SVEN_HARNESS_LOCAL_ENDPOINT=<url of your server>`. One without the other is
  a skip.
- The default suite only validates this sample and checks that it skips.

## Run it (only when you have a model serving)

```sh
sven harness validate samples/harnesses/08-local-qwen          # offline, always safe
SVEN_HARNESS_LOCAL_MODEL=1 SVEN_HARNESS_LOCAL_ENDPOINT=http://localhost:11434 \
    sven harness test samples/harnesses/08-local-qwen          # or: make harness/test/local
```

## Expected

Without the opt-in, `sven harness test samples/harnesses/08-local-qwen`
reports one skipped case and exits 0. With it, the smoke case completes with an
output of the form `{"sentiment": "positive"}`; the expectation is loose on
purpose, because a real model's wording varies.
