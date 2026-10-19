<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Tool definitions

Every tool definition (name, description, JSON parameter schema) is sent on
every model request and sits in every recorded trajectory, so each token in it
is paid again on each turn. The definitions are therefore kept small and their
total size is pinned by a test. Deferred disclosure
([tool-disclosure.md](tool-disclosure.md)) removes most definitions from a
request; this page is about the size of the definitions themselves.

## Measuring

`sven_turn::ledger::schema_tokens` estimates one definition as the bytes of
its name, its description and its serialized parameter schema, each divided by 4
and rounded up. It is the estimator behind the token ledger
([token-ledger.md](token-ledger.md)); no tokenizer is linked, so it is
deterministic and provider-independent rather than exact. Use it to compare two
versions of a definition, not to predict a bill.

`crates/sdk/tests/tool_footprint.rs` builds an engine with `Toolset::coding()`
and `Toolset::research()` against an empty home directory, reads the
definitions the model was offered on its first request, and prints the
per-tool table with `--nocapture`:

```
cargo test -p sven-sdk --test tool_footprint -- --nocapture
```

The test fails when the total exceeds its ceiling: `CODING_CEILING` (3,055
estimated tokens) and `RESEARCH_CEILING` (2,390). The ceilings sit just above
the totals of the default build, which has the most tool text (image-capable
`read_file` and `attach_file`); a build with fewer features is smaller. A
definition can only grow if the change raises the ceiling too, which makes the
growth a visible decision.

The research set is the coding set without `edit_file`, `write_file`, `shell`
and `attach_file`. `skill` is measured with no skills installed: its description
does not list them, because the system prompt already does. The set is also
pinned by name in `crates/bootstrap/src/registry.rs`: adding or removing a
built-in tool fails `the_agent_tool_set_is_what_we_think_it_is` until the list
there is changed on purpose.

## Rules the definitions follow

- A description says what the tool does and what the schema cannot: behaviour,
  limits, pitfalls. It does not repeat parameter defaults the schema states,
  and it never names another tool (`a_tool_description_never_names_another_tool`
  enforces this; a compound tool may name its own actions, and cross-tool policy
  belongs to the system prompt).
- `required` lists exactly the parameters a call cannot do without. A parameter
  with a default is optional.
- A default stated in text is the default the implementation uses.
- Every tool states a one-line `summary` (at most 100 characters), which is what
  the model reads about a tool whose definition it has not loaded.
- Tool and parameter names are fixed: they are part of recorded trajectories
  and of what models have been trained on.
- Everything the system prompt names in backticks as a tool exists
  (`the_system_prompt_only_names_tools_that_exist`).

## What each built-in tool can reach

For an embedding application that runs unattended episodes in a project root
(`EngineBuilder` with a confined `PathScope`). "Jailed" means the tool resolves
its paths through the scope and refuses anything outside the root.

| tool | in `coding()` | files | network and other processes |
|------|---------------|-------|------------------------------|
| `read_file`, `find_file`, `write_file`, `edit_file`, `grep` | yes | jailed | none |
| `attach_file` | yes (`media` feature) | jailed | audio the model cannot take is sent to a speech-to-text service over D-Bus, and only in a build with the `asr` feature |
| `shell` | yes | starts in the root, and a `workdir` must stay inside it; the command itself is not confined | anything the process can do |
| `web_fetch`, `web_search` | yes | none | any http(s) URL; no private-address filter |
| `memory` | yes | its own notes file in the user's config directory (or `tools.memory.memory_file`), not the project; reads `.sven/knowledge/` | none |
| `semantic_memory` | yes (`memory` feature) | its own SQLite store | none |
| `skill` | yes | reads installed skill files | none |
| `todo` | yes | none (the list lives in the session) | none |
| `task` | yes | sub-agents get the same jailed tools | the session's model; a child process per sub-agent |
| `ask_question`, `system` | yes | `system` can write the MCP server list in a configuration file | `system` can add an MCP server, which starts a command, in `agent` mode only |
| `context` | no (full tool set only); with addressable context, an `expand`-only `context` | `open` is jailed since the scope reaches it, and a directory walk skips symbolic links | `query` and `reduce` send file chunks to the session's own model, no other model |
| `gdb` | no (full tool set only) | `executable` is only checked for existence, not jailed | connects to any `host:port`; runs `gdb_path` and, for `start_server`, any command via `sh -c`; a command matching `tools.deny_patterns` is refused |
