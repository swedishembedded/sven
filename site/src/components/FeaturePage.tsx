// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// One of the site's feature pages (src/pages.ts): what sven does there, an
// example to type, and the way to start or to read more. Static markup only,
// so the page reads the same before and after the scripts load.

import type { Page } from '../pages'
import { PAGES } from '../pages'
import { SIGN_UP_URL } from '../links'

export default function FeaturePage({ page }: { page: Page }) {
  const others = PAGES.filter((other) => other.path !== page.path)
  return (
    <main className="pt-28 pb-24">
      <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
        <p className="text-xs font-mono uppercase tracking-widest text-accent-blue">{page.eyebrow}</p>
        <h1 className="mt-4 max-w-3xl text-4xl sm:text-5xl font-extrabold tracking-tight text-text-primary leading-[1.1]">
          {page.heading}
        </h1>
        <p className="mt-6 max-w-2xl text-lg text-text-secondary leading-relaxed">{page.lede}</p>
        <div className="mt-8 flex flex-wrap gap-3">
          <a href={SIGN_UP_URL} className="btn-primary">
            Start with sven
          </a>
          <a href={page.docs.href} className="btn-secondary">
            {page.docs.label}
          </a>
        </div>

        {page.example ? (
          <div className="mt-12 max-w-2xl rounded-lg border border-bg-border bg-bg-elevated px-4 py-3 font-mono text-sm overflow-x-auto">
            <span style={{ color: '#5b8dee' }}>$ </span>
            <span className="text-text-primary">{page.example}</span>
          </div>
        ) : null}

        <div className={`mt-16 grid gap-10 ${page.image ? 'lg:grid-cols-2 items-start' : ''}`}>
          <ul className="grid gap-6">
            {page.points.map((point) => (
              <li key={point.title} className="rounded-xl border border-bg-border bg-bg-card p-6">
                <h2 className="text-lg font-semibold text-text-primary">{point.title}</h2>
                <p className="mt-2 text-text-secondary leading-relaxed">{point.body}</p>
              </li>
            ))}
          </ul>
          {page.image ? (
            <img
              src={page.image.src}
              alt={page.image.alt}
              width={800}
              height={500}
              className="w-full h-auto rounded-xl border border-bg-border"
            />
          ) : null}
        </div>

        <nav aria-label="More of what sven does" className="mt-20">
          <h2 className="text-sm font-semibold uppercase tracking-widest text-text-dim">More of what sven does</h2>
          <ul className="mt-4 grid gap-3 sm:grid-cols-3">
            {others.map((other) => (
              <li key={other.path}>
                <a
                  href={other.path}
                  className="block rounded-xl border border-bg-border bg-bg-card p-4 hover:border-accent-blue/40 transition-colors"
                >
                  <span className="block font-medium text-text-primary">{other.eyebrow}</span>
                  <span className="mt-1 block text-sm text-text-secondary">{other.heading}</span>
                </a>
              </li>
            ))}
          </ul>
        </nav>
      </div>
    </main>
  )
}
