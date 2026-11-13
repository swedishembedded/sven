<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# independent-agents

Scope: Running sven as an independent actor on a network an embedding application controls: egress control, an API tool, and per-run state, with nothing shared between agents on one machine.

## Done

- `network:` section / `--allow-host`, `--deny-egress`, `--egress-proxy`,
  `--ca-bundle`, `--resolve`: process-wide egress policy enforced at the
  connection layer of every client built through `sven_http::client_builder`
  (deny by default when `allow_hosts` is present; IP literals and redirects
  covered; sub-agents inherit it). The model provider's client is the one
  exemption (`model_client_builder`). `make check/gates/confined-io` keeps
  crates from building clients or asking `dirs::` themselves.
- `http_request` tool: any method, headers, body, streamed read with a limit.
- `--state-root DIR` (`sven-roots`): configuration, data, state, `~`, temp
  files and `/etc/sven` handled per process; sub-agents inherit it;
  `tests/state_root_isolation.rs` runs two agents concurrently.
- `--disable-tool NAME` on the top-level command (`shell` for a run with no
  command execution).
- Documented in `docs/guides/independent-agents.md`.

## Left

- One egress policy and one state root per process. Several engines with
  different policies in one process need a scoped policy (a task-local the
  client builder reads); today the SDK refuses a second, different policy.
- `sven mcp serve` and the standalone `sven-acp`/`sven-mcp` binaries read no
  `--state-root`/`network` flags except `sven acp serve`; they use the
  configuration file (under a state root set by the embedder).
- Shell commands and stdio MCP servers are outside the HTTP policy; the
  embedder's sandbox or namespace must contain them. A Landlock profile
  with a network deny for the shell exists only for hosted runs.
- The Brave search endpoint is a fixed name: emulating it needs `resolve`
  plus a `ca_bundle`.
