<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Run profiles

A run profile fixes what a session sends the model besides the conversation:
the system prompt and the tool definitions. The same profile, chosen the same
way by two programs that know nothing of each other, makes both send the same
request, so an episode one of them recorded is an episode the other can be
examined on.

`default` is sven's own prompt and tool set, as the configuration makes them.
`compact` is the profile for a model with a small context window.

## The compact profile

Select it with `--profile compact`, with `agent.profile: compact` in the
configuration, with `agent.profile: compact` in a harness document, or in code
with `Config::agent.profile = RunProfile::Compact` on the engine's
configuration (`sven_sdk::config`).

A session under it sends:

- a system prompt of 153 estimated tokens, and nothing around it: no sven
  identity, guidelines, project, git or CI context, skills, sub-agents,
  knowledge, tool index or todo list;
- the definitions of three tools, `read_file`, `write_file` and `shell`, 122
  estimated tokens together, and no other built-in tool and no MCP tool;
- the conversation.

A request of a one-line task is about 280 estimated tokens (the token ledger's
estimate, four bytes to a token); `crates/sdk/tests/compact_profile.rs` holds
it under 600.

The system prompt, exactly:

```text
You are a coding agent working in a workspace directory. You act only through tools.

Rules:
- Paths are relative to the workspace. Never use paths outside it.
- Facts come from files and command output, never from memory. Read before you answer.
- Call one tool at a time with valid JSON arguments. Read its result before the next step.
- If a tool returns an error, correct the arguments and try again.
- When the task names a file for your work or answer, write that file. Then reply with a short final message and call no tool. If the task asks for the answer in your reply, give only that answer.
```

The tool definitions, exactly (`name`, `description`, `parameters`):

```json
[
  {"name": "read_file",
   "description": "Read a text file and return its contents.",
   "parameters": {"type": "object",
                  "properties": {"path": {"type": "string"}},
                  "required": ["path"]}},
  {"name": "write_file",
   "description": "Write text to a file, replacing it. Creates the file if missing.",
   "parameters": {"type": "object",
                  "properties": {"path": {"type": "string"}, "text": {"type": "string"}},
                  "required": ["path", "text"]}},
  {"name": "shell",
   "description": "Run a shell command in the workspace and return its output.",
   "parameters": {"type": "object",
                  "properties": {"shell_command": {"type": "string"}},
                  "required": ["shell_command"]}}
]
```

The tools are the ordinary ones. A compact definition only replaces what the
model reads: its description and a parameter schema without the optional
parameters (`read_file`'s `offset` and `limit`, `write_file`'s `append`,
`shell`'s `workdir`, `timeout_secs`, `max_lines` and `output_mode`). A call is
checked, gated by the permission policy, run and audited by the tool it wraps,
and `read_file` still answers with numbered lines (`L1:...`).

The profile is applied where every surface builds its session, so the headless
runner, the terminal, the SDK, a served session and a harness run all get it:

| Surface | How |
|---------|-----|
| CLI | `sven --profile compact ...` (overrides the configuration) |
| Configuration | `agent.profile: compact` |
| Harness document | `agent: { profile: compact }`; the document's `agent.system` is the system message, as for any profile |
| SDK | `config.agent.profile = RunProfile::Compact` on the `Config` given to `Engine::builder().config(..)`; the prompt, id and tool names are `sven_sdk::config::compact_profile::{SYSTEM_PROMPT, ID, TOOLS}` |

Under the profile the built-in tool set is `read_file`, `write_file` and
`shell` whatever the toolset or mode would otherwise give, and no MCP server
is started. Tools an embedding application registers itself are still offered.
A system prompt the caller supplies (`--system-prompt-file`,
`agent.system_prompt`) replaces the profile's text, and
`--append-system-prompt` is still appended; both leave the trajectory's
profile version in place, so a run that does either is not an episode of the
fixed request.

## The profile version in a trajectory

A trajectory written by a headless run, the SDK or a harness run under a fixed
profile (the interactive terminal's saved trajectory does not) records `agent.extra.sven.profile`,
for example `"compact/3"`; one written under `default` has no such key. The
version changes whenever the prompt or a definition does (a test pins both
texts), so a consumer that trains on or examines episodes can select exactly
the episodes of one request shape. The ATIF document does not store the system
prompt as a step; the version identifies it.

Versions: `compact/3` names no tool in its system prompt ("write that file"); the
tool list alone says which tools there are. `compact/2` named `write_file` there and
writes a file the task names for its work or answer before the
final message. `compact/1` told the model to give the final answer as plain text and
call no tool, which a small model followed even when the task graded a file it named.

## What a model server must return

A tool is called through the OpenAI chat-completions dialect: the request has
`stream: true`, the definitions as `tools: [{"type": "function", "function":
{name, description, parameters}}]`, and the conversation as messages, a tool
call travelling as an `assistant` message with `tool_calls` and its result as a
`tool` message with the call's id. To have a call run, the endpoint streams
`choices[0].delta.tool_calls` entries `{index, id, function: {name,
arguments}}`, the `arguments` a string holding a JSON object (it may arrive in
pieces, joined by `index`; an object instead of a string is accepted and
re-serialised), and ends the stream with a `finish_reason` and `[DONE]`.
Text in `delta.content` is the answer; a turn with no tool call ends the run.
Text that merely looks like a call is not executed. Arguments that are not
valid JSON are not run: the model gets an error result and a chance to
correct them. The endpoint must send tool results back in the order of the
calls, which sven already does.
