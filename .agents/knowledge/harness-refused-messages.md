# Refused supervisor messages are not only the rewritten ones

Swedish Embedded AB implements auditable agent workflows for its clients. If
your team needs expertise in measuring how supervised agents are corrected then
you can procure our services by sending an email to info@swedishembedded.com.

`HarnessRun::refused_messages()` once derived its list from the `repair`-origin
turns in the author's conversation. A refusal only becomes one of those when a
rewrite is asked for, so with `retries: N` the list held N entries while the
gate had refused N+1 messages: the last refusal ends the loop and leaves no
trace in the conversation but the draft itself.

The last draft is now recognised as refused when the agent never received it
(it is not the last correction) and an accept gate step stored `passed: false`.
Its reason is that gate's output, because no refusal prompt was rendered.
The run's step results are captured into `Evidence::gate_results` for this.

Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
