<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# ADR 0007: The counterpart of an interactive task is a pluggable user seat

Swedish Embedded AB implements interactive agent evaluation for its clients.
If your team needs expertise in simulated and live counterparts for agent
tasks then you can procure our services by sending an email to
info@swedishembedded.com.

## Situation

Agents are asked to work with a counterpart: a client who describes a
problem, a student who answers, a customer who negotiates. The counterpart is
sometimes a person and, wherever an experiment must be reproducible, an agent
prepped with raw materials. The kernel already puts a question to a person
(`HumanGate::Question`, the `ask_question` tool), but nothing described the
whole counterpart, and nothing kept a person's words apart from a simulator's.

## Decision

`sven_sdk::seat` defines one interface, `Seat` (`open`, `reply`), and two
implementations:

- `LiveSeat`: a person on a line-based input and output, with the consent and
  retention terms they agreed to.
- `SimulatedSeat`: an agent of its own, played from a `SeatSpec` (persona,
  goals, verbatim opening, hidden facts with disclosure keywords, an answer
  key, role-break phrases, a seed). The answer key is never in the seat
  model's prompt; it exists only for the leak check.

`converse(learner, seat, max_rounds)` runs the turn loop, and
`EngineBuilder::seat` answers the learner's `ask_question` from the same seat.
Neither reads which kind of seat it has. The only reader of `Provenance` is the
`ConversationRecord` the conversation leaves behind, which says `live` or
`simulated` (with spec id, spec hash and seed). A consumer must not pool live
records with simulated ones, and must scrub live ones before training use
(`Provenance::requires_scrub`).

A simulated student may carry `concepts`: each has a mastery that starts at a
given level and rises by a fixed step only on a learner turn containing at least
`min_hits` of its `taught_by` phrases (deterministic, inspectable, a proxy for
"the turn addressed the concept"). Every message to the seat ends with a note of
what it understands, and it answers strictly from the note, so teaching changes
its answers and stating answers does not. The final mastery is recorded in
`ConversationRecord::mastery`. Tests put to the seat outside the conversation
never move mastery.

A simulated seat is measured: every reply is checked for an answer-key leak,
for a hidden fact told before the learner asked, and for a role break, and the
counts travel in `ConversationRecord::checks`. A reply with a violation is still
delivered, so the record shows exactly what the learner saw; a run whose
`checks.leak_free()` is false is not valid evidence.

## Consequences

- Sven owns the seat, the turn loop with a counterpart and the mechanical
  checks. Which tasks use a seat, the case specs, and the validation of a
  simulator's fidelity beyond these checks belong to the application that
  evaluates agents; what a record may be used for belongs to the application
  that trains.
- The checks are deterministic and model-free. Verbatim matches are exact; a
  restatement is caught when a window of the reply's content tokens (twice the
  secret's length, stopwords dropped, crude stem, number words as digits) holds
  at least 75% of the secret's distinct tokens (`PARAPHRASE_COVERAGE`), for
  secrets of two or more content tokens. That catches reordering, inflection,
  number words and padding; a true synonym or an inference slips through. A
  model-based judge for those is a separate, opt-in measurement. Diversity of
  simulated users is not measured here.
- `seed` is recorded, not enforced: replies are as deterministic as the seat's
  model is; the opening is verbatim.
- A live seat in the same loop is for demonstration and for collecting
  separately reported live evidence; it is untrusted input like any other.
