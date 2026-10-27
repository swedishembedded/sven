// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// The browser build in a real browser: a module Web Worker loads sven's web
// package, runs a session whose model is a fake OpenAI-compatible endpoint
// on the same origin, and gives the agent a tool of the worker's own. The
// model asks for the tool, the worker's handler answers, and the answer that
// follows reaches the page. Chrome is driven over its DevTools protocol with
// Node's own WebSocket, so the test needs no package of its own.
//
//   CHROME=<chrome or chromium binary> SVEN_WASM_PKG=<the web build> \
//     node --test crates/wasm/tests/browser.mjs
// Run by `make test/wasm/browser`.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";

const chrome = process.env.CHROME;
const pkg = process.env.SVEN_WASM_PKG;
assert.ok(chrome, "CHROME names a Chrome or Chromium binary");
assert.ok(pkg, "SVEN_WASM_PKG names the web build of sven-wasm");

const WORKER = `
import init, { SvenSession } from "/pkg/sven_wasm.js";
await init();
const calls = [];
const session = new SvenSession(JSON.stringify({
  base_url: self.location.origin + "/v1",
  model: "fake/model",
  tools: [{ name: "page_title", description: "The page's title.",
            parameters: { type: "object", properties: {} }, effect: "read" }],
}), async (name, args) => { calls.push({ name, args }); return "Inbox"; });
let text = "";
session.onEvent((json) => {
  const event = JSON.parse(json);
  if (event.TextDelta !== undefined) text += event.TextDelta;
  if (event === "TurnComplete") self.postMessage({ text, calls });
});
await session.send("what is the page called?");
`;

const PAGE = `<!doctype html><meta charset="utf-8"><title>sven-wasm</title>`;

function sse(chunks) {
  return chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n";
}

/// The site the browser loads: the page, the worker, the package, and the model.
async function serve(requests) {
  const types = { ".js": "text/javascript", ".wasm": "application/wasm", ".json": "application/json" };
  const server = createServer((req, res) => {
    const url = new URL(req.url, "http://localhost");
    if (url.pathname === "/") return res.writeHead(200, { "content-type": "text/html" }).end(PAGE);
    if (url.pathname === "/worker.js") return res.writeHead(200, { "content-type": "text/javascript" }).end(WORKER);
    if (url.pathname.startsWith("/pkg/")) {
      const file = path.basename(url.pathname);
      try {
        const body = readFileSync(path.join(pkg, file));
        return res.writeHead(200, { "content-type": types[path.extname(file)] ?? "application/octet-stream" }).end(body);
      } catch {
        return res.writeHead(404).end();
      }
    }
    if (url.pathname === "/v1/chat/completions") {
      let body = "";
      req.on("data", (part) => (body += part));
      req.on("end", () => {
        requests.push(JSON.parse(body));
        const answer =
          requests.length === 1
            ? sse([
                {
                  choices: [{
                    index: 0,
                    delta: { tool_calls: [{ index: 0, id: "call_1", type: "function",
                                            function: { name: "page_title", arguments: "{}" } }] },
                    finish_reason: null,
                  }],
                },
                { choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
              ])
            : sse([
                { choices: [{ index: 0, delta: { content: "It is called Inbox." }, finish_reason: null }] },
                { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
              ]);
        res.writeHead(200, { "content-type": "text/event-stream" }).end(answer);
      });
      return;
    }
    res.writeHead(404).end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return server;
}

/// Starts headless Chrome and resolves to its DevTools address.
function launch(profile) {
  const child = spawn(
    chrome,
    ["--headless=new", "--no-sandbox", "--no-first-run", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  const address = new Promise((resolve, reject) => {
    let log = "";
    child.stderr.on("data", (part) => {
      log += part;
      const found = log.match(/DevTools listening on ws:\/\/([^/]+)\//);
      if (found) resolve(found[1]);
    });
    child.on("exit", (code) => reject(new Error(`Chrome exited (${code}): ${log}`)));
  });
  return { child, address };
}

/// One DevTools session on a page: send a command, await its result.
async function devtools(webSocketUrl) {
  const socket = new WebSocket(webSocketUrl);
  await new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = () => reject(new Error("cannot reach Chrome's DevTools"));
  });
  let last = 0;
  const waiting = new Map();
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    waiting.get(message.id)?.(message);
    waiting.delete(message.id);
  };
  return {
    send(method, params = {}) {
      last += 1;
      const id = last;
      return new Promise((resolve, reject) => {
        waiting.set(id, (message) => (message.error ? reject(new Error(message.error.message)) : resolve(message.result)));
        socket.send(JSON.stringify({ id, method, params }));
      });
    },
    close: () => socket.close(),
  };
}

test("a session runs in a real browser's worker and its tool runs there too", async () => {
  const requests = [];
  const server = await serve(requests);
  const profile = mkdtempSync(path.join(tmpdir(), "sven-wasm-chrome-"));
  const { child, address } = launch(profile);
  try {
    const origin = `http://127.0.0.1:${server.address().port}`;
    const target = await (await fetch(`http://${await address}/json/new?${origin}/`, { method: "PUT" })).json();
    const page = await devtools(target.webSocketDebuggerUrl);
    await page.send("Page.enable");
    for (let i = 0; i < 50; i++) {
      const { result } = await page.send("Runtime.evaluate", { expression: "document.readyState", returnByValue: true });
      if (result.value === "complete" && (await page.send("Runtime.evaluate", { expression: "location.origin", returnByValue: true })).result.value === origin) break;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    const evaluated = await page.send("Runtime.evaluate", {
      expression: `new Promise((resolve, reject) => {
        const worker = new Worker("/worker.js", { type: "module" });
        const timer = setTimeout(() => reject(new Error("no answer from the worker")), 20000);
        worker.onmessage = ({ data }) => { clearTimeout(timer); resolve(data); };
        worker.onerror = (event) => { clearTimeout(timer); reject(new Error("worker failed: " + event.message)); };
      })`,
      awaitPromise: true,
      returnByValue: true,
    });
    page.close();
    assert.equal(evaluated.exceptionDetails, undefined, JSON.stringify(evaluated.exceptionDetails));
    const { text, calls } = evaluated.result.value;

    assert.equal(text, "It is called Inbox.");
    assert.deepEqual(calls, [{ name: "page_title", args: "{}" }]);
    assert.equal(requests.length, 2);
    assert.ok(requests[0].tools.some((tool) => tool.function.name === "page_title"));
    const result = requests[1].messages.find((m) => m.role === "tool");
    assert.equal(result?.tool_call_id, "call_1");
    assert.match(JSON.stringify(result.content), /Inbox/);
  } finally {
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill();
    await exited;
    server.close();
    rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
