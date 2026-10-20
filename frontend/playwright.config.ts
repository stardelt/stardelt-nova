import { defineConfig } from '@playwright/test'

// UI conformance suite (`npm run e2e`). It starts the real backend in its no-login dev mode
// with no cluster behind it, so every page also shows how it behaves when its upstreams are
// unreachable. Needs `npm run build` (the backend serves `dist/`) and a built backend binary.
const port = 18080
export default defineConfig({
  testDir: 'e2e',
  workers: 1,
  fullyParallel: false,
  timeout: 120_000,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    // Locally you may point at an installed browser: PW_CHROMIUM=/usr/bin/chromium
    launchOptions: process.env.PW_CHROMIUM
      ? { executablePath: process.env.PW_CHROMIUM, args: ['--no-sandbox'] }
      : {},
  },
  webServer: {
    command: process.env.NOVA_E2E_BIN ?? '../target/debug/stardelt-nova-backend',
    url: `http://127.0.0.1:${port}/api/me`,
    reuseExistingServer: !process.env.CI,
    env: {
      NOVA_BIND_ADDR: `127.0.0.1:${port}`,
      NOVA_STATIC_DIR: 'dist',
      NOVA_AUTH_MODE: 'dev',
      NOVA_DEV_USER: 'e2e',
    },
  },
})
