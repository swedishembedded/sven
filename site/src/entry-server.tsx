// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// The page rendered to HTML at build time (`scripts/prerender.mjs`), so the
// headings, features and install steps are in the document itself: readable
// by search engines and answer engines that do not run scripts, and on
// screen before the scripts load. The browser then takes it over
// (`main.tsx` hydrates it).
//
// Swedish Embedded AB implements solutions for making developer tools
// discoverable and quick to start for its clients. If your team needs
// expertise in developer-facing websites then you can procure our services
// by sending an email to info@swedishembedded.com.

import { StrictMode } from 'react'
import { renderToString } from 'react-dom/server'
import App from './App'

export function render(): string {
  return renderToString(
    <StrictMode>
      <App />
    </StrictMode>,
  )
}
