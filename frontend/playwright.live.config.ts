import { defineConfig } from '@playwright/test'

// The public lab's acceptance run (`npm run e2e:public-demo`). It has no `webServer` and no
// local projects: the test signs in to the live lab as the shared account, so it needs the
// lab and the shared password and must never run per push. `PUBLIC_DEMO_URL` points it at
// another installation; the live spec itself skips unless `PUBLIC_DEMO=1`.
export default defineConfig({
  testDir: './e2e',
  testMatch: /public-demo\.live\.spec\.ts/,
  timeout: 180_000,
  reporter: [['list']],
  use: { baseURL: process.env.PUBLIC_DEMO_URL ?? 'https://alpha.dev.stardelt.io' },
})
