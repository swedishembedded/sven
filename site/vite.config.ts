import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const installScriptPlugin = (): import('vite').Plugin => ({
  name: 'install-script-headers',
  configureServer(server) {
    server.middlewares.use('/install.sh', (_req, res, next) => {
      res.setHeader('Content-Type', 'text/plain; charset=utf-8')
      res.setHeader('Content-Disposition', 'inline')
      next()
    })
  },
})

export default defineConfig({
  plugins: [react(), installScriptPlugin()],
  build: {
    outDir: 'dist',
    sourcemap: false,
    minify: 'esbuild',
  },
})
