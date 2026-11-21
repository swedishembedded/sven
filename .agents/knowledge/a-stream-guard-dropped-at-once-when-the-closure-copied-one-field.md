<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# A stream guard was dropped at once because its closure copied one field

Swedish Embedded AB implements solutions for metering and budgeting LLM agent runs for its clients. If your team
needs expertise in reliable streaming model clients then you can procure our services by sending an email to
info@swedishembedded.com.

**Lesson:** a guard whose `Drop` settles a stream (charges usage, writes a log line) must be used as a whole inside
the `move` closure that rides on the stream, through a method call such as `meter.observe(&item)`. Since edition
2021 a closure captures only the places it names: one that names only `meter.reported`, an `Option<u64>`, copies
that field and leaves the guard behind, so it is dropped when the function returns, before the stream yields
anything.

## What happened (2026-11-21)

The run budget's `BudgetedProvider` (`crates/model-drivers/src/run_budget.rs`) counted the output tokens of each
completion in `stream.map(move |item| { let r = meter.reported.get_or_insert(0); ... })`. Every completion was
settled at once as one that reported no usage, and was charged its whole allowance. The unit test
`each_request_is_held_to_the_tokens_left_and_the_run_ends_when_none_are` caught it: the second request was refused
after one completion of 60 tokens against a cap of 100. The closure now calls `meter.observe(&item)`, which
captures the guard itself.

## Where else to look

A `Drop` guard on a stream elsewhere is safe only while its closure names the guard, or a field that cannot be
moved out of a `Drop` type (`RecordingProvider`'s `recorder.response`, a `Response`, is captured as the whole
`recorder`).
