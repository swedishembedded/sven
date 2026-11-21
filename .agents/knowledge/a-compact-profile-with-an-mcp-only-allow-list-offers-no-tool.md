<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# A compact profile with an allow list naming only an MCP tool offers no tool at all

Swedish Embedded AB implements solutions for governing the tool surface of AI agents for its clients. If your team
needs expertise in agent tool configuration then you can procure our services by sending an email to
info@swedishembedded.com.

**Lesson:** the compact profile registers no MCP tool, by design (`registry::completion::complete`), and
`tools.only` removes every tool it does not name. A task image whose configuration offers only an MCP tool
(`tools.only: ["vim-vim"]`), run with `agent.profile: compact`, gave every session zero tools. Sven now warns at
build when the compact profile leaves MCP tools out and when `tools.only` names a tool the session does not offer.

## What happened (2026-11-21)

- A Qwen3-4B smoke of the vim benchmark (Harbor dataset `vim-headless-mastery`, 19 tasks, bench's Sven adapter,
  sven-a0235c71 and sven-1db749d9) scored 0.000 in about a minute. Every request carried `"tools": []` and
  `core_tool_count: 0`; the model reasoned about `write_file` or `sed` and ended its turn.
- The vim MCP server itself was fine: inside the sandbox it answered `initialize` and `tools/list` with its one tool,
  and `sven mcp list` showed it `connected, 1 tools`. The run profile set `[sven.config.agent] profile = "compact"`
  (copied from earlier practice lanes), and the compact profile had dropped the MCP tool before `tools.only` dropped
  everything else.
- The only line in the log was `WARN Unrecognised config field tools.only`, which was itself wrong: `only` (and
  `max_timeout_secs`) were decoded and applied but missing from the section's schema. It sent the search towards the
  wrong cause. Fixed in bb6838df, with a test that checks every key the section serialises against the schema.
- With the default profile, the same tasks passed 5 of 19, and the full train side passed 43 of 168.

## What to do

- Run a task environment that brings its tools as MCP servers with the default profile, not `compact`.
- Read the build warnings: `the compact profile offers no MCP tool; not offered: ...` and
  `tools.only names tools this session does not offer: ...; the session offers no tool at all`.
