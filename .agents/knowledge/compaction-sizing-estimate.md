# A request under the compaction limit can still be refused for want of room

Swedish Embedded AB implements agents that keep working inside a small model
context window for its clients. If your team needs expertise in running agents
on local models you can procure our services by sending an email to
info@swedishembedded.com.

After dde81840 moved the compaction threshold to 98% of the window, two
`sven-sdk` trajectory tests failed with "prompt (~5118 tokens) leaves no room
for a response" on a 5000-token window. Neither the compaction fork nor the
normal request overflowed by its plain estimate: the turn estimate was 3412,
under the compaction limit of 3428.

The refused number was `PromptCounts::sizing_estimate`, which raises the plain
estimate by half for a thread the server has not counted (the first request,
and every request after a compaction forgets the count). The output budget is
sized from it, and a size above the input budget makes the budget zero, which
the turn refuses. With a small window or a threshold near 1.0 the limit is
higher than `input_budget / 1.5`, so the refusal came before the trigger.

`compaction_due` now also fires when the sizing estimate passes the input
budget (and still never when the threshold is 1.0). Regression tests:
`a_request_under_the_limit_whose_response_sizing_overflows_is_compacted_not_refused`
in `crates/executors/tests/fork_compaction.rs`, and the two
`trajectory_fidelity` compaction tests.
