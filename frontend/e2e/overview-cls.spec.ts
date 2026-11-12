import { test, expect, type Route } from '@playwright/test'

// The Catalogs card reserves the height of the table it replaces, so nothing below it moves
// when the numbers land (stardelt-nova#512 item 2). The guard measures the cumulative layout
// shift the page produces while `/api/catalog/metrics` is still in flight and compares it with
// the design's budget. Every `/api/**` call is stubbed, so the test needs no cluster.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' }

/** Three real catalogs: the card ends in a table with a header and three rows. */
const METRICS = {
  catalogs: [
    { name: 'bronze', schemas: 2, tables: 12, sizeBytes: 1048576, records: 1000, lastCommitMs: 1 },
    { name: 'silver', schemas: 2, tables: 12, sizeBytes: 1048576, records: 1000, lastCommitMs: 1 },
    { name: 'gold', schemas: 2, tables: 12, sizeBytes: 1048576, records: 1000, lastCommitMs: 1 },
  ],
  totals: { catalogs: 3, schemas: 6, tables: 36, records: 3000, sizeBytes: 3145728 },
  generatedAt: 0,
}

/** Every API call the page makes; the metrics answer is 600 ms late so the skeleton is visible. */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/catalog/metrics') {
      await new Promise((resolve) => setTimeout(resolve, 600))
      return json(route, METRICS)
    }
    // List endpoints, not `{}`: the shell and the page read them as arrays.
    if (path === '/api/jobs') return json(route, [])
    if (path === '/api/apps/links') return json(route, [])
    // static-dev is the operator here, so the page also renders the Platform apps card.
    if (path === '/api/apps') return json(route, [])
    return json(route, {})
  })
}

test('the Overview shifts less than 0.02 while the Catalogs card loads', async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as { __cls: number; __entries: number }
    w.__cls = 0
    w.__entries = 0
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries() as unknown as Array<{ value: number; hadRecentInput: boolean }>) {
        if (!entry.hadRecentInput) {
          w.__cls += entry.value
          w.__entries += 1
        }
      }
    }).observe({ type: 'layout-shift', buffered: true })
  })
  await stubApi(page)
  await page.goto('/')
  await expect(page.getByRole('link', { name: 'bronze' })).toBeVisible()
  await page.waitForTimeout(300) // let the card settle
  const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls)
  expect(cls, `CLS ${cls}`).toBeLessThan(0.02)
})
