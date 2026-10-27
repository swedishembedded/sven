<!--
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

SPDX-License-Identifier: Apache-2.0
-->

# Agent sessions in a browser

`sven-wasm` runs a sven agent session inside a web page, a Web Worker or any
JavaScript runtime. It is not a second implementation: the session is the
same reactive agent machine, kernel loop, turn executor and tool executor
the `sven` binary runs, compiled to WebAssembly. `make test/wasm` runs the
same tests natively and as WebAssembly and requires the same results.

## Build

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <the wasm-bindgen in Cargo.lock>
make build/wasm
```

This writes `target/wasm-pkg/web` (for a page or worker, an ES module) and
`target/wasm-pkg/node` (for Node). Each has a `package.json` whose `sven`
field names the version, git commit and remote-control protocol it was built
from; install it from there with your package manager rather than copying
files, and check `sven.protocol` before use.

## Use

```js
import init, { SvenSession } from "sven-wasm";

await init();
const session = new SvenSession(JSON.stringify({
  base_url: "/api/v1",        // an OpenAI-compatible API
  model: "some/model",
}));
session.onEvent((json) => render(JSON.parse(json)));
await session.send("What can you do?");
```

- `new SvenSession(options)` starts a session. `options` is JSON with
  `base_url` (an OpenAI-compatible API ending in `/v1`), `model`, and
  optionally `api_key` and `history`: the conversation so far as
  `[{"role": "user" | "assistant", "text": "..."}]`, oldest first, so a
  reopened conversation continues where it stopped. Prefer an endpoint of the page's own origin that the
  page's session cookie authenticates: then no key is ever in the page.
- `onEvent(callback)` delivers every event from then on: sven's own
  `SessionEvent` as a JSON string (`{"TextDelta":"..."}`,
  `{"ToolCallStarted":{...}}`, `"TurnComplete"`, ...). Events a slow page
  missed arrive as one `{"EventsDropped":{"count":n}}`.
- `send(text)` sends the person's message and resolves to whether the
  session took it; `cancel()` stops the answer being written.

## What differs from a native session

- The model is reached over the runtime's own `fetch`.
- There are no native tools: a page has no shell or file system, and a
  session offers no tools until the page registers its own.
- The audit log and the verifiers are not available; an effect that needs
  them is answered as undeliverable.
- A session lives as long as the page or worker that runs it.
