// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// The JavaScript face of a sven session, end to end: the wasm build loaded
// the way a page loads it, a model reached over the runtime's own fetch, and
// sven's events delivered to the page's callback. The model is a fake
// OpenAI-compatible server streaming one answer; everything else is real.
//
//   SVEN_WASM_PKG=<dir with the nodejs build> node --test crates/wasm/tests/session.mjs
// Run by `make test/wasm`.

import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";

const pkg = process.env.SVEN_WASM_PKG;
assert.ok(pkg, "SVEN_WASM_PKG names the nodejs build of sven-wasm");
const { SvenSession } = createRequire(import.meta.url)(path.resolve(pkg, "sven_wasm.js"));

function sse(res, chunks) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  for (const chunk of chunks) res.write(`data: ${JSON.stringify(chunk)}\n\n`);
  res.end("data: [DONE]\n\n");
}

async function serveOneAnswer(requests) {
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (part) => (body += part));
    req.on("end", () => {
      requests.push({ url: req.url, auth: req.headers.authorization, body: JSON.parse(body) });
      sse(res, [
        { choices: [{ index: 0, delta: { content: "ok" }, finish_reason: null }] },
        { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
      ]);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return server;
}

test("a session with no key sends none, for an endpoint its page's cookie authenticates", async () => {
  const requests = [];
  const server = await serveOneAnswer(requests);
  try {
    const session = new SvenSession(
      JSON.stringify({ base_url: `http://127.0.0.1:${server.address().port}/v1`, model: "fake/model" }),
    );
    const finished = new Promise((resolve) => {
      session.onEvent((json) => {
        if (JSON.parse(json) === "TurnComplete") resolve();
      });
    });
    await session.send("hi");
    await finished;
    assert.equal(requests.length, 1);
    assert.equal(requests[0].auth, undefined);
  } finally {
    server.close();
  }
});

test("a session answers through the runtime's fetch and reports sven's own events", async () => {
  const requests = [];
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (part) => (body += part));
    req.on("end", () => {
      requests.push({ url: req.url, auth: req.headers.authorization, body: JSON.parse(body) });
      const delta = (content) => ({ choices: [{ index: 0, delta: { content }, finish_reason: null }] });
      sse(res, [
        delta("Hello "),
        delta("from the browser."),
        { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
      ]);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  try {
    const session = new SvenSession(
      JSON.stringify({
        base_url: `http://127.0.0.1:${port}/v1`,
        model: "fake/model",
        api_key: "k",
        history: [
          { role: "user", text: "my name is Ada" },
          { role: "assistant", text: "Hello, Ada." },
        ],
      }),
    );
    const events = [];
    const finished = new Promise((resolve) => {
      session.onEvent((json) => {
        const event = JSON.parse(json);
        events.push(event);
        if (event === "TurnComplete") resolve();
      });
    });
    assert.equal(await session.send("hi"), true);
    let timer;
    await Promise.race([
      finished,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`no TurnComplete: ${JSON.stringify(events)}`)), 10_000);
      }),
    ]);
    clearTimeout(timer);

    const text = events.filter((e) => e.TextDelta !== undefined).map((e) => e.TextDelta).join("");
    assert.equal(text, "Hello from the browser.");
    assert.equal(requests.length, 1);
    assert.equal(requests[0].url, "/v1/chat/completions");
    assert.equal(requests[0].auth, "Bearer k");
    assert.equal(requests[0].body.model, "fake/model");
    assert.equal(requests[0].body.stream, true);
    // The conversation so far reaches the model before the new message.
    const turns = requests[0].body.messages.filter((m) => m.role !== "system");
    assert.deepEqual(turns, [
      { role: "user", content: "my name is Ada" },
      { role: "assistant", content: "Hello, Ada." },
      { role: "user", content: "hi" },
    ]);
  } finally {
    server.close();
  }
});
