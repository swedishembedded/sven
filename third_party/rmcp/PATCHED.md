<!-- SPDX-License-Identifier: Apache-2.0 -->

# rmcp 3.8.0, patched

The upstream crate (https://github.com/modelcontextprotocol/rust-sdk/,
Apache-2.0, `LICENSE` here) as published on crates.io as 3.8.0, sources only,
with one change, used by sven through `[patch.crates-io]` in the workspace
`Cargo.toml`. Drop this copy once a release carries the fix.

**The bug.** A server result is decoded through `ServerResult`, an untagged
enum that tries the known result types before `CustomResult`.
`CallToolResult`'s decoder accepted any object carrying one of its known
fields, `_meta` included. MCP servers now stamp `_meta`
(`io.modelcontextprotocol/serverInfo`) on every result, so the result of any
custom method (MCP's `files/authorizeUpload`, `files/authorizeDownload`)
decoded as an empty tool result and lost every field.

**The change** (`src/model.rs`, `impl Deserialize for CallToolResult`): `_meta`
alone no longer makes a tool result; one of `content`, `structuredContent` or
`isError` must be there. sven's test: `crates/mcp-client/tests/it/files.rs`
(the server answers as an MCP server SDK does, `_meta` included).
