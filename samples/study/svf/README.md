# sample: study/svf

Does reading a document make an agent able to do something it could not do
before - and does that ability survive the document going away?

```bash
make samples/study/svf/build
make samples/study/svf/run ARGS="--errata none --report a0.json"
make samples/study/svf/run ARGS="--errata real --report a1.json"
```

## What it demonstrates

* **A framework claim.** The sample depends on `sven-sdk` and nothing else in
  the workspace. One `Engine` is built and twenty `Agent`s run on it - the
  split the framework exists for, used rather than described.
* **An experiment that can fail.** Arms A0 and A3 are *required* to score
  badly. An experiment whose every arm can only succeed measures nothing.
* **Hygiene that is checked, not assumed.** A leak of the secret into the
  workspace does not make a run fail; it makes a run *succeed*, for the wrong
  reason, and look like the result you were hoping for. So the sample refuses
  to start an errata-free arm whose workspace contains a secret.
* **Instances that are independent.** The workspace is reset to its documents
  before every instance. Every header in this format opens with the same secret
  bytes, so one correct answer left in the directory would hand the signature to
  the next instance, which would then be scored as knowing something it read off
  the floor.

## The task

SVF is a byte container invented for this sample. `SVF.md` specifies its
structure completely and deliberately omits four values: a 4-byte signature, a
length bias, a body-order rule, and a CRC-8 polynomial and initial value. They
live in `SVF-ERRATA.md`.

An agent is asked for the SVF encoding of a payload, as lowercase hex, written
to a JSON file. Twenty instances in three tiers, each needing strictly more of
the errata than the one above it:

| tier | needs | instances |
|---|---|---|
| `header` | signature + length bias | 6 |
| `preamble` | + body order | 6 |
| `frame` | + CRC parameters | 8 |

So the score says *which* piece of knowledge landed. 6/20 means the signature
and the bias and nothing else.

The values are invented, so an agent that produces a correct artifact either
read the errata or got lucky at odds of at best one in 2^32 - that is the
signature alone, and a frame needs the checksum parameters too. A real format,
however obscure, would leave "it already knew" as an unfalsifiable alternative
explanation for every positive result.

## The arms

The arm is a pair: which documents the workspace contains (`--errata`), and
which model the engine reaches (ordinary sven configuration, so
`brain serve --openai` is just a configured endpoint). The same binary runs all
four.

| arm | `--errata` | model | required result | what its absence would allow |
|---|---|---|---|---|
| A0 | `none` | base | **fails** | the task is guessable, and every later number is noise |
| A1 | `real` | base | **passes** | the errata is insufficient; nothing downstream is interpretable |
| A2 | `none` | trained on the errata | *the measurement* | - |
| A3 | `none` | trained on the decoy | **no better than A0** | "any training helps" is indistinguishable from learning |

Run A0 and A1 first. They need no training at all, and if either comes out
wrong the task is wrong - stop and fix it rather than training against it.

## What this sample does not do

* **It does not train.** Turning `SVF-ERRATA.md` into training data and gating
  a candidate adapter is brain's `samples/study/document`, which reads a frozen
  `{fact, probe_question, expected_answer}` set and publishes an adapter only
  if it clears a pre-registered bar. This sample produces the *score* that
  makes such an adapter worth publishing or not.
* **It does not rule out retrieval.** An agent that writes the errata into its
  own memory store and reads it back has not learned anything, and looks
  identical from here. Separating the two needs the four-configuration
  ablation - weights old/new against memory wiped/retained - described in
  `.agents/roadmap/document-to-capability.md`.
* **It does not check every secret.** The hygiene scan covers the signature and
  the CRC parameters, which are high-entropy strings. The length bias and the
  body-order rule are prose; scanning for `3` or `reversed` would report a leak
  on every document. Reviewing a new document by eye for those two is a manual
  step, and this paragraph is where that is written down.
* **It is not an RL environment.** Grading is exact-match on a hash-like
  answer, so a near miss and a wild guess score the same. That is what makes
  arm A0 meaningful and it is also why there is no gradient to train against
  here. Training data comes from the document, not from these rollouts.

## Output

Per instance, one line. At the end, a per-tier and total score, and with
`--report FILE.json` a machine-readable result including the workspace
manifest and the hygiene check's finding.
