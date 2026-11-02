// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Renders every page into the built site after `vite build`: the server
// build of `src/entry-server.tsx` renders the home page into dist/index.html
// and each feature page (src/pages.ts) into dist/<path>.html, each with its
// own title, description, canonical address and preview, and writes the
// sitemap from the same list. Fails if the template is not as expected, so a
// change to index.html cannot silently ship an empty or mislabelled page.

import { readFileSync, rmSync, writeFileSync } from 'node:fs'

const ORIGIN = 'https://agentsven.com'
const EMPTY_ROOT = '<div id="root"></div>'
const { render, PAGES } = await import(new URL('../dist-ssr/entry-server.js', import.meta.url))

const template = readFileSync('dist/index.html', 'utf8')
if (!template.includes(EMPTY_ROOT)) {
  console.error(`dist/index.html has no ${EMPTY_ROOT} to fill`)
  process.exit(1)
}

const escape = (text) =>
  text.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')

/// The template with one tag's content replaced; fails if the tag is gone.
function setTag(html, pattern, replacement) {
  if (!pattern.test(html)) throw new Error(`index.html lost ${pattern}`)
  return html.replace(pattern, replacement)
}

function pageHtml(page) {
  let html = template
  const title = escape(page.title)
  const description = escape(page.description)
  const url = `${ORIGIN}${page.path}`
  html = setTag(html, /<title>[^<]*<\/title>/, `<title>${title}</title>`)
  html = setTag(html, /<meta name="description" content="[^"]*" \/>/, `<meta name="description" content="${description}" />`)
  html = setTag(html, /<link rel="canonical" href="[^"]*" \/>/, `<link rel="canonical" href="${url}" />`)
  html = setTag(html, /<meta property="og:url" content="[^"]*" \/>/, `<meta property="og:url" content="${url}" />`)
  html = setTag(html, /<meta property="og:title" content="[^"]*" \/>/, `<meta property="og:title" content="${title}" />`)
  html = setTag(html, /<meta property="og:description" content="[^"]*" \/>/, `<meta property="og:description" content="${description}" />`)
  html = setTag(html, /<meta name="twitter:title" content="[^"]*" \/>/, `<meta name="twitter:title" content="${title}" />`)
  html = setTag(html, /<meta name="twitter:description" content="[^"]*" \/>/, `<meta name="twitter:description" content="${description}" />`)
  return html.replace(EMPTY_ROOT, `<div id="root">${render(page.path)}</div>`)
}

writeFileSync('dist/index.html', template.replace(EMPTY_ROOT, `<div id="root">${render('/')}</div>`))
for (const page of PAGES) writeFileSync(`dist${page.path}.html`, pageHtml(page))

const today = new Date().toISOString().slice(0, 10)
const urls = ['/', ...PAGES.map((page) => page.path)]
writeFileSync(
  'dist/sitemap.xml',
  `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls
    .map((path) => `  <url>\n    <loc>${ORIGIN}${path === '/' ? '/' : path}</loc>\n    <lastmod>${today}</lastmod>\n  </url>`)
    .join('\n')}\n</urlset>\n`,
)

rmSync('dist-ssr', { recursive: true, force: true })
console.log(`prerendered ${urls.length} pages: ${urls.join(' ')}`)
