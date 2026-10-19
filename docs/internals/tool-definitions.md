# Tool definitions

Every tool definition (name, description, JSON parameter schema) is sent on
every model request and sits in every recorded trajectory, so each token in it
is paid again on each turn. The definitions are therefore kept small and their
size is pinned by a test.

## Measuring

`sven_turn::ledger::schema_tokens` estimates one definition as the bytes of
its name, its description and its serialized parameter schema divided by 4,
rounded up. It is the estimator behind the token ledger
([token-ledger.md](token-ledger.md)); no tokenizer is available in the
workspace, so it is deterministic and provider-independent rather than exact.
Use it to compare two versions of a definition, not to predict a bill.

`crates/sdk/tests/tool_footprint.rs` builds an engine with `Toolset::coding()`
and `Toolset::research()` against an empty home directory, reads the
definitions the model was offered, and prints this table with `--nocapture`:

```
cargo test -p sven-sdk --test tool_footprint -- --nocapture
```

The same test fails when the total exceeds its ceiling (`CODING_CEILING`,
`RESEARCH_CEILING`). The ceilings sit just above the totals measured in the
default build, which has the most tool text (image-capable `read_file` and
`attach_file`); a build with fewer features is smaller. A definition can only
grow if the change raises the ceiling too.

The `task` growth (+26) is the brief and scope fields (`evidence`,
`deliverable`, `paths`) and the `ledger` action offered with addressable
context, which a model cannot guess from the other fields. The `skill` growth (+27) is the `search` action and its `query`
parameter, which is how a model finds a skill the prompt's compact index does
not name.

## Current size

Estimated tokens per request, bytes / 4. "Before" is the tree before the
definitions were tightened.

| tool | before | now |
|------|-------:|----:|
| task | 591 | 484 |
| grep | 399 | 265 |
| system | 395 | 293 |
| edit_file | 385 | 199 |
| todo | 324 | 226 |
| find_file | 296 | 216 |
| ask_question | 261 | 210 |
| shell | 252 | 185 |
| memory | 247 | 197 |
| read_file | 219 | 158 |
| attach_file | 196 | 146 |
| write_file | 193 | 136 |
| web_fetch | 140 | 97 |
| skill | 137 | 131 |
| web_search | 137 | 97 |
| **coding total** | **4172** | **3040** |
| **research total** | **3146** | **2374** |

The research set is the coding set without `edit_file`, `write_file`, `shell`
and `attach_file`. `skill` is measured with no skills installed: its description
does not list them, because the system prompt already does.

## Rules the definitions follow

- A description says what the tool does and what the schema cannot: behaviour,
  limits, pitfalls. It does not repeat parameter defaults the schema states,
  and it never names another tool (a test enforces this; cross-tool policy
  belongs to the system prompt).
- `required` lists exactly the parameters a call cannot do without. A parameter
  with a default is optional.
- A default stated in text is the default the implementation uses.
- Tool and parameter names are fixed: they are part of recorded trajectories
  and of what models have been trained on.

## What each built-in tool can reach

For an embedding application that runs unattended episodes in a project root
(`EngineBuilder` with a confined `PathScope`). "Jailed" means the tool resolves
its paths through the scope and refuses anything outside the root.

| tool | in `coding()` | files | network and other processes |
|------|---------------|-------|------------------------------|
| `read_file`, `find_file`, `write_file`, `edit_file`, `grep` | yes | jailed | none |
| `attach_file` | yes | jailed (`attach_file.rs`, `resolve_for` on `path`; registered with the scope in `registry.rs`) | audio the model cannot take is sent to a speech-to-text service over D-Bus, and only in a build with the `asr` feature |
| `shell` | yes | starts in the root; the command itself is not confined | anything the process can do |
| `web_fetch`, `web_search` | yes | none | any http(s) URL; no private-address filter |
| `memory` | yes | its own notes file in the user's config directory, not the project | none |
| `skill` | yes | reads installed skill files | none |
| `todo` | yes | none (the list lives in the session) | none |
| `task` | yes | sub-agents get the same jailed tools | the session's model |
| `ask_question`, `system` | yes | `system` can write the MCP server list in a configuration file | `system` can add an MCP server, which starts a command |
| `context` (open, read, grep, query, reduce) | no (full profile only) | `open` is jailed since the scope reaches it, and a directory walk skips symbolic links | `query` and `reduce` send file chunks to the session's own model, no other model |
| `gdb` | no (full profile only) | `executable` is only checked for existence, not jailed | connects to any `host:port`; runs `gdb_path` and, for `start_server`, any command via `sh -c` |

Before the scope reached `context_open`, a session in the full profile could
open any readable path through it, and a link inside the project could lead a
directory walk out of it.
