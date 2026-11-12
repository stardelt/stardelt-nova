import { test, expect, type Route } from '@playwright/test'

// The Catalogs card reserves its height while the numbers are in flight, then shows the
// table (stardelt-nova#588, slice 1 of #512). Every `/api/**` call is stubbed, so the test
// needs no cluster.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' }

/** An instance with nothing to browse yet: the card ends in "No catalogs to show.". */
const METRICS = {
  catalogs: [],
  totals: { catalogs: 0, schemas: 0, tables: 0, records: 0, sizeBytes: 0 },
  generatedAt: 0,
}

/** Every API call the page makes; the metrics answer is 500 ms late so the skeleton is visible. */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/catalog/metrics') {
      await new Promise((resolve) => setTimeout(resolve, 500))
      return json(route, METRICS)
    }
    // List endpoints, not `{}`: the shell and the page read them as arrays.
    if (path === '/api/jobs') return json(route, [])
    if (path === '/api/apps/links') return json(route, [])
    // The Overview's charts read list pages: an empty page, not `{}` (nova#883).
    if (path === '/api/query-log') return json(route, { items: [], nextCursor: null })
    if (path === '/api/runs') return json(route, { runs: [], groups: [] })
    // static-dev is the operator here, so the page also renders the Platform apps card.
    if (path === '/api/apps') return json(route, [])
    return json(route, {})
  })
}

test('the Catalogs card shows eight skeleton rows, then the table', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  const skeletons = page.locator('.skeleton')
  await expect(skeletons).toHaveCount(8)
  await expect(page.getByText('No catalogs to show.')).toBeVisible()
  await expect(skeletons).toHaveCount(0)
})

test('reduced motion removes the shimmer', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await stubApi(page)
  await page.goto('/')
  const skeleton = page.locator('.skeleton').first()
  await expect(skeleton).toBeVisible()
  await expect
    .poll(() => skeleton.evaluate((el) => getComputedStyle(el).animationName))
    .toBe('none')
})
