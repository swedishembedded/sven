// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// The shared conversation document from JavaScript: replicas that write
// offline, exchange only what each lacks, and a late joiner that replays a
// server's stored updates, all reach the same conversation. Run by
// `make test/wasm` (SVEN_WASM_PKG names the nodejs build).

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";

const pkg = process.env.SVEN_WASM_PKG;
assert.ok(pkg, "SVEN_WASM_PKG names the nodejs build of sven-wasm");
const { ConversationDoc } = createRequire(import.meta.url)(path.resolve(pkg, "sven_wasm.js"));

const message = (id, role, text, state = "complete") => ({
  id,
  role,
  author: role === "user" ? "user_ada" : "agent",
  state,
  parts: JSON.stringify([{ type: "text", text }]),
});

test("replicas writing offline converge, and a late joiner replays the server's log", () => {
  const server = new ConversationDoc();
  const log = []; // what a server keeps: every update it accepted, in arrival order
  const publish = (from) => {
    const update = from.diff(server.stateVector());
    server.apply(update);
    log.push(update);
  };

  const terminal = new ConversationDoc();
  const tab = new ConversationDoc();
  terminal.addMessage(JSON.stringify(message("m1", "user", "list files")));
  publish(terminal);
  tab.apply(server.diff(tab.stateVector()));

  // Both write while apart.
  terminal.addMessage(JSON.stringify(message("m2", "assistant", "README.md", "streaming")));
  tab.addMessage(JSON.stringify(message("m3", "user", "and the tests?")));
  terminal.updateMessage("m2", "complete", JSON.stringify([{ type: "text", text: "README.md, Cargo.toml" }]));
  publish(tab);
  publish(terminal);
  terminal.apply(server.diff(terminal.stateVector()));
  tab.apply(server.diff(tab.stateVector()));

  const everywhere = JSON.parse(server.messages());
  assert.deepEqual(JSON.parse(terminal.messages()), everywhere);
  assert.deepEqual(JSON.parse(tab.messages()), everywhere);
  assert.equal(everywhere.length, 3);
  assert.equal(everywhere[0].id, "m1");
  assert.equal(everywhere.find((m) => m.id === "m2").state, "complete");

  // Someone opens the conversation later: the stored log, in reverse, twice.
  const late = new ConversationDoc();
  for (const update of [...log].reverse().concat(log)) late.apply(update);
  assert.deepEqual(JSON.parse(late.messages()), everywhere);

  assert.throws(() => late.apply(new Uint8Array([1, 2, 3])));
});
