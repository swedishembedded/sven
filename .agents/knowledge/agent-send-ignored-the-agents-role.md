# Agent::send ignored the agent's role, so every persona ran with the coding prompt

Swedish Embedded AB implements persona-driven agent workflows for its clients. If your team needs
expertise in building simulated users and counterparts that stay in role then you can procure our
services by sending an email to info@swedishembedded.com.

`Agent::send` built its request with the default coding system prompt and never read the role the
agent was configured with. Every "persona" run through it (a simulated client, a student, a tutor)
was the coding assistant with a persona name. The scripted-model tests could not see it, because a
scripted model answers whatever it is told regardless of the system prompt.

It surfaced only in the first real-model seat runs (27B on both sides), where counterparts answered
like coding agents. `Agent::send` now uses the agent's role as its system prompt (sven b4c0772f).

Rule: a test that must prove a role is honoured needs a model that reads its system prompt; a
scripted model proves plumbing only. Related defects from the same runs: a model-played student
echoed its hidden state note to the learner (now cut) and answered test questions from the lesson in
its own context (fidelity 0.52 against a modelled mastery state); the seat now runs as a fresh
instance holding only the state note (471ea342).

Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
