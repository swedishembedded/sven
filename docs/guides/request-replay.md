<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

Swedish Embedded AB implements solutions for measuring how a prompt change
alters an LLM agent's next decision, in minutes instead of whole benchmark
runs, for its clients. If your team needs expertise in prompt and tool
definition tuning for coding agents then you can procure our services by
sending an email to info@swedishembedded.com.
-->

# Replaying a request with a variant

Tuning the prompt or a tool description by running whole tasks takes hours per
comparison. sven records every model request exactly as sent, knows which named
prompt sections and tool definitions each one is made of, and can send one
again with a section or a tool swapped, as often as you like. The cost of a
comparison is the number of samples, at one decision.

## Record

```sh
sven --record-requests run.requests.jsonl --headless "fix the failing test"
```

The file gets one `request` line per model call (sub-agents included) and one
`response` line with what came back. Each request carries a manifest: the
hash of the system text, the byte span and SHA-256 of every named prompt
section, a hash per tool definition (name, description and parameters in
canonical JSON) and one over the ordered tool list. The manifest is for the
record: no provider ever receives it. A request whose system prompt a
compaction rewrote carries no manifest, because its sections would describe
text that is no longer there; its tools can still be swapped.

## Inspect

```sh
sven replay run.requests.jsonl --list
```

```
  14  messages=31   seed=- in=24510 cached=24100 out=312 calls=[grep({"pattern":...})] text="..."
      identity#178976b8 mode#b4033878 project#978142c4 tool_index#dfc0e0cc skills#e208f848 guidelines.working_method#01fbdb91 guidelines.tools#bcbe52e7 | tools=23
```

The number is what `--request` takes. Each section id is followed by the
first eight digits of its hash, so two recordings show at a glance which
sections differ. The ids are `identity`, `mode`, `project`, `git`,
`project_instructions`, `tool_index`, `skills`, `agents`, `knowledge`,
`knowledge_drift`, `ci`, `guidelines.working_method`, `guidelines.tools` and
`append`; a custom system prompt is `custom`. A block with nothing to say has
no section.

## Swap

A variant is a YAML file; every key is optional.

```yaml
name: tools-guidance-shorter       # shown in the output; defaults to the file name
system_sections:
  - id: guidelines.tools
    replace: |
      ### Tools
      Read before you edit. Prefer one precise search to several broad ones.
  - id: knowledge
    remove: true
tools:
  - name: grep
    description: "Search file contents for a regular expression."
    parameter_descriptions:        # top-level parameters, by name
      pattern: "A regular expression."
  - name: find_file
    remove: true
append_message:
  role: user                       # or assistant
  text: "Continue."
```

A `replace` takes the place of the section's text but keeps the blank lines
that separate it from the section before, so write it as plain prose;
`remove` takes the whole section. Everything outside the named sections and
tools is byte-identical to the recording. A section or tool the request does
not have is an error that lists the ones it does have.

## Replay

```sh
sven replay run.requests.jsonl --request 14 --variant tools-guidance-shorter.yaml \
  --repeat 10 --json -M openai/my-model > variant.jsonl
sven replay run.requests.jsonl --request 14 --repeat 10 --json -M openai/my-model > baseline.jsonl
```

The request goes to the model that `-M` and the configuration select, with the
sampling settings it was recorded with (`--seed` and `--reported-seed` pin the
seed). No tool runs. With `--json` each sample is one line:

```json
{"variant":"tools-guidance-shorter","request":14,"sample":3,"calls":[{"name":"grep","arguments":["path","pattern"],"valid":true,"errors":[]}],"text":"","output_tokens":212,"reasoning_chars":604,"error":null}
```

`calls` lists the tools the model called, the names of the arguments it
passed, and whether the arguments satisfy that tool's parameter schema as the
replayed request offered it. A run without `--variant` is named `baseline`.
Count calls per tool and invalid calls over the lines of two files to compare
the variants:

```sh
jq -r '.calls[].name' variant.jsonl | sort | uniq -c
jq -s 'map(.calls[] | select(.valid | not)) | length' variant.jsonl
```

A comparison at one decision point says how the model's next step changes, not
whether a task is solved sooner; confirm a winner on tasks before adopting it.
Reference: [`sven replay`](../reference/cli.md#sessions-and-trajectories).
