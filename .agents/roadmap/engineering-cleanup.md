<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# engineering-cleanup

Scope: configuration that is declared once and checked strictly (the section schemas, the model section's layering, a strict check for embedding applications), and the file-size gate that `make check` fails on.

Audit of 2026-11-21 against sven 7a9f60cb (read-only; every file and line below was read). Three defects of the last
week share one cause: a configuration fact declared twice, once as the type that decodes it and once by hand
(`tools.only` missing from its schema, bb6838df; `driver_options` and then every other field of a model section lost to
provider expansion, 43f84ae9 and 7a9f60cb; the prompt naming tools the session did not offer, 0d94638e). Each was fixed
where it surfaced. This ledger removes the second declaration. Execution waits for the safety nets each item names.

## Order

EC-1 first (it unblocks every lane's `make check`). EC-2 and EC-3 are independent. EC-4 builds on EC-2.

## EC-1 `crates/remote/src/client.rs` fails ARCH-007 (S, open)

- **Problem.** 1094 lines, over the 800 production-line limit and not in `[[allow.large_file]]`, so `make check`
  fails for every change. It holds four things: server discovery (`normalize_server`, `discover`), five error types,
  `RemoteClient`'s environment calls (`attach_environment`, `detach_environment`, `whoami`, `commands`) and its
  conversation calls (`sync_conversation`, `upload_to_conversation`, `send_to_conversation`, `decide_approval`,
  `post_events`), plus a second `impl RemoteClient` block.
- **Target.** `client/discovery.rs`, `client/errors.rs`, `client/environment.rs`, `client/conversation.rs` under a
  `client/mod.rs` that keeps `RemoteClient` and re-exports the same public paths. A move, no behaviour change; no
  allowlist entry.
- **Safety net.** The crate's existing tests, unchanged; `cargo public-api` (or a `use` smoke test of every public
  path) to show the surface did not move.

## EC-2 One declaration per configuration section (M, open)

- **Problem.** About twenty sections describe their keys twice: the serde type that decodes them and a hand-written
  `Schema` (`crates/config/src/schema.rs`) that only reports unknown keys, in `bootstrap/src/config/{mod,agent,tools,
  context,cost,listing,delegation,disclosure,ocr}.rs`, `model-drivers/src/config.rs`, `mcp-client/src/config.rs`,
  `decision/src/config.rs`, `tools-web/src/config.rs`, `tools-ctx/src/memory.rs`, `frontend/src/settings.rs` and
  others. When they drift, a key that is applied is reported as unknown (`tools.only`, which sent the vim smoke's
  diagnosis the wrong way: knowledge "a compact profile with an MCP-only allow list offers no tool") or a key that is
  ignored is reported as known. One section (`tools`) has a test that checks its serialised keys against its schema.
- **Target.** The schema derived from the types. `schemars` is already a workspace dependency: a section's known keys
  are the properties of `schema_for!(Section)` (honouring renames, aliases and flattening); `Kind::Ignored` becomes a
  field attribute read from the derived schema's extensions. The hand-written `schema()` functions are deleted;
  `Schema::report` walks the derived schema.
- **Safety net (first).** One test over every section: its `Default`, and a fully populated value, serialised and
  checked to report no unknown key; one corpus test of real configuration files (an examiner's rendered config, a task
  image's `.sven.yaml`, the documented examples) whose warnings are pinned before the change and equal after.

## EC-3 The model section as a typed patch (S/M, done)

- **Problem.** `config_settle.rs::overlay_requested` (lines 165-193) lays a `model:` section over a named provider's
  entry by round-tripping both through `serde_json` and treating a field as "set" when it differs from
  `ModelConfig::default()`. A field set explicitly to its default value is therefore lost: every plain `bool`
  (`cache_system_prompt`, `cache_tools`, `cache_conversation`, `cache_images`, `cache_tool_results` default `true`;
  `extended_cache_time`, `sampling_best_effort` default `false`) cannot override an entry that holds the other value.
  If either serialisation or the re-decode fails, the function returns or keeps the unmerged value without a word.
- **Target.** `ModelSection`, every field `Option<T>` (a patch, decoded from the file), and
  `entry.to_model_config(name).apply(&section)`: set means present in the file. `driver_options` keep their key-by-key
  merge. No JSON round trip, nothing that can fail silently.
- **Safety net.** A table test in `config_tests.rs`: for every field, an entry holding one value and a section setting
  the other, including the default, and the section's value wins; a section that sets nothing leaves the entry whole.

## EC-4 A strict check for applications that embed sven (S/M, open; needs EC-2)

- **Problem.** An unknown key, an ignored section, a `tools.only` that names a tool the session does not offer, and
  the compact profile dropping a configured MCP server's tools are warnings. In a headless run started by another
  program the warning lands in a log no one reads; the vim smoke scored 0.000 with every request carrying no tools.
- **Target.** `sven config check FILE [--strict]` printing every finding and exiting non-zero under `--strict`, and a
  `--strict-config` run flag that turns the same findings into a refusal before the first request. Interactive use
  keeps warnings. An embedding application validates the configuration it renders once, before it starts trials.
- **Safety net.** One test per finding kind for each of check, `--strict` and `--strict-config`.

## Not now

- **Splitting every file the line counter lets through.** Many files are long because their tests are inline
  (`count_production_lines` skips `#[cfg(test)]` items); the gate measures what matters. Split a file when a change
  gives a reason.
- **`SVEN_MOCK_RESPONSES` beside `model.mock_responses_file`:** two channels for one setting, but the environment one is
  how the CLI tests reach a mock; retire it together with those tests' harness, not alone.
