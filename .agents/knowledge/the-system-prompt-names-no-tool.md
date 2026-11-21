<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# The system prompt names no tool

Swedish Embedded AB implements solutions for keeping agent prompts consistent with the tools an agent really has for
its clients. If your team needs expertise in agent tool configuration then you can procure our services by sending an
email to info@swedishembedded.com.

**Lesson:** a system prompt that names tools is wrong in some session, because the tools a session has vary
(`tools.only`, `tools.disabled`, `--no-tools`, deferred disclosure, MCP servers). Bench's vim environment
(`tools.only: ["vim-vim"]`) showed the cost. In 433 of 576 verified passing trajectories (sven-1db749d9,
Qwen3.8-27B-FP8), the model still called `shell` (472 calls) or `read_file` (11 calls), which the Guidelines had named,
and each call got `tool 'shell' is not enabled for this session`. The first fix (0d94638e, `CoreTools`) made the
prompt depend on which built-in tools were offered, so the prompt builder held tool names and a branch for each tool.
That was replaced: tools now reach the model only through the tool list, and each tool's own description carries its
advice.

## How it works now

- The system prompt (`crates/turn/src/prompts.rs`) has one generic `### Tools` block (section id `guidelines.tools`):
  use the available tools as appropriate to complete the task correctly, with the fewest steps and tool calls, and pass
  a timeout for long-running commands when a tool supports one. No mode sentence, catalog section (skills, subagents,
  knowledge) or per-turn announcement names a tool or promises a capability ("run shell commands").
- Tool-specific advice is in the owning tool's description: `shell` says to use it for builds, tests, git and running
  programs and to pass `timeout_secs` for long runs; `grep` says to read only the relevant ranges afterwards; `skill`
  says to load at most one skill per task; `ask_question` says to use it for an ambiguous request; `plan_tasks` holds
  what the "Parallel Work" paragraph said.
- Under deferred disclosure, the index of tools that are not loaded is part of the `tools` tool's description,
  not a `## Deferred Tools` section of the prompt. It is fixed once, after the name policy and the preloads are
  applied (`tool_discovery::fix_index`). The token ledger counts it under `tool_schemas`.
- The `compact` profile is now `compact/3`: its prompt says "write that file" instead of naming `write_file`.
- Spec: `crates/bootstrap/tests/it/prompt_tool_agnostic.rs` checks the names held by real registries (all tools, one
  MCP tool, no tools, deferred) against the prompt the model received. `system_prompt_golden.rs` pins every prompt;
  the `custom` and `custom+append` digests did not change.

## Experiments

Runs that compare against the frozen `sven-7a9f60cb` binary stay on it until their comparison ends. Later
experiments adopt a build with this change at an experiment boundary, never in the middle of one: the prompt, the
tool descriptions and the `compact` profile version all changed.
