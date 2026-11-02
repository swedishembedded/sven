// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Puts the rendered page into dist/index.html after `vite build`: the server
// build of `src/entry-server.tsx` renders it, and the result replaces the
// empty root element. Fails if the root is not where it is expected, so a
// change to index.html cannot silently ship an empty page again.

import { readFileSync, rmSync, writeFileSync } from 'node:fs'

const EMPTY_ROOT = '<div id="root"></div>'
const { render } = await import(new URL('../dist-ssr/entry-server.js', import.meta.url))

const html = readFileSync('dist/index.html', 'utf8')
if (!html.includes(EMPTY_ROOT)) {
  console.error(`dist/index.html has no ${EMPTY_ROOT} to fill`)
  process.exit(1)
}
const body = render()
writeFileSync('dist/index.html', html.replace(EMPTY_ROOT, `<div id="root">${body}</div>`))
rmSync('dist-ssr', { recursive: true, force: true })
console.log(`prerendered dist/index.html (${(body.length / 1024).toFixed(1)} KiB of markup)`)
