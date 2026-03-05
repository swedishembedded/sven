import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const GITHUB_REPO = 'swedishembedded/sven'
const FALLBACK_VERSION = 'latest'

async function fetchLatestVersion(): Promise<string> {
  try {
    const res = await fetch(
      `https://api.github.com/repos/${GITHUB_REPO}/releases/latest`,
      { headers: { Accept: 'application/vnd.github+json' } },
    )
    if (!res.ok) return FALLBACK_VERSION
    const data = (await res.json()) as { tag_name?: string }
    return data.tag_name ?? FALLBACK_VERSION
  } catch {
    return FALLBACK_VERSION
  }
}

export default defineConfig(async () => {
  const latestVersion = await fetchLatestVersion()
  console.log(`[vite] sven latest release: ${latestVersion}`)

  return {
    plugins: [react()],
    define: {
      __LATEST_VERSION__: JSON.stringify(latestVersion),
    },
    build: {
      outDir: 'dist',
      sourcemap: false,
      minify: 'esbuild',
    },
  }
})
