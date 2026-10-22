import { readFileSync } from 'node:fs'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// The version the footer shows is the build's own: this package's version, read at build
// time. No endpoint, no request — the running build already knows what it is.
const { version } = JSON.parse(
  readFileSync(new URL('./package.json', import.meta.url), 'utf8'),
) as { version: string }

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },
  server: {
    port: 5173,
    proxy: {
      // /api/* during local dev proxies to the Rust backend on 8080
      '/api': {
        target: 'http://localhost:8080',
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: true,
  },
})
