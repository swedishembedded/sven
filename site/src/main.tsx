import { StrictMode } from 'react'
import { createRoot, hydrateRoot } from 'react-dom/client'
// Fonts are served from this site, never fetched from a third party, so a
// visit sends nothing about the visitor anywhere else.
import '@fontsource/inter/400.css'
import '@fontsource/inter/500.css'
import '@fontsource/inter/600.css'
import '@fontsource/inter/700.css'
import '@fontsource/inter/800.css'
import '@fontsource/jetbrains-mono/400.css'
import '@fontsource/jetbrains-mono/500.css'
import './index.css'
import App from './App'

const root = document.getElementById('root')!
const app = (
  <StrictMode>
    <App path={window.location.pathname.replace(/\/$/, '') || '/'} />
  </StrictMode>
)
// The built page arrives already rendered (scripts/prerender.mjs); the dev
// server's does not.
if (root.hasChildNodes()) hydrateRoot(root, app)
else createRoot(root).render(app)
