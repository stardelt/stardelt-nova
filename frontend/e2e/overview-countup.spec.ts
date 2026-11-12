import { test, expect, type Route } from '@playwright/test'

// The Overview why-count tiles count 0 → their total once, over 400 ms (#512 item 4).
// Every `/api/**` call is stubbed, so the test needs no cluster.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' }

const METRICS = {
  catalogs: [],
  totals: { catalogs: 4, schemas: 9, tables: 1234, records: 0, sizeBytes: 0 },
  generatedAt: 0,
}

/** Every API call the page makes, answered at once so only the count-up moves. */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/catalog/metrics') return json(route, METRICS)
    // List endpoints, not `{}`: the shell and the page read them as arrays.
    if (path === '/api/jobs') return json(route, [])
    if (path === '/api/apps/links') return json(route, [])
    if (path === '/api/apps') return json(route, [])
    if (path === '/api/settings') return json(route, {})
    return json(route, {})
  })
}

test('the Tables count reaches its total', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  const tables = page.locator('.card', { hasText: 'Tables' }).first()
  await expect(tables).toContainText('1,234', { timeout: 3_000 })
})

test('reduced motion shows the total at once', async ({ page }) => {
  await stubApi(page)
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.goto('/')
  const tables = page.locator('.card', { hasText: 'Tables' }).first()
  await expect(tables).toContainText('1,234', { timeout: 3_000 })
})
