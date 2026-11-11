import { test, expect, type Page, type Route } from '@playwright/test'

// The Overview's freshness card (stardelt-nova#510, slice 4b): the five tables the person
// read most and when each last changed. Every `/api/**` call is stubbed, so no cluster is
// needed and the "changed" and "stale" lines are deterministic.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' }

const METRICS = {
  catalogs: [],
  totals: { catalogs: 0, schemas: 0, tables: 0, records: 0, sizeBytes: 0 },
  generatedAt: 0,
}

const READ_TABLES = {
  tables: [
    { namespace: 'mobility.silver', name: 'yellow_trips', queries: 42, lastRead: '2026-01-01T00:00:00Z', lastWrite: new Date().toISOString() },
    { namespace: 'mobility.gold', name: 'kpis', queries: 18, lastRead: '2026-01-01T00:00:00Z', lastWrite: '2026-01-01T00:00:00Z' },
    { namespace: 'shop.silver', name: 'orders', queries: 9, lastRead: '2026-01-01T00:00:00Z' },
  ],
}

async function stubApi(page: Page, opts: { readTablesStatus?: number } = {}) {
  await page.route('**/api/**', (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/catalog/metrics') return json(route, METRICS)
    if (path === '/api/lineage/read-tables') {
      if (opts.readTablesStatus) return json(route, { error: 'Could not read freshness.' }, opts.readTablesStatus)
      return json(route, READ_TABLES)
    }
    // List endpoints, not `{}`: the shell and the page read them as arrays.
    if (path === '/api/jobs') return json(route, [])
    if (path === '/api/apps/links') return json(route, [])
    if (path === '/api/apps') return json(route, [])
    return json(route, {})
  })
}

/** The card, found by its heading. */
const freshnessCard = (page: Page) =>
  page.locator('section.card', { has: page.getByRole('heading', { name: 'Freshness' }) })

test('the card lists the tables read most, newest write first', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  const card = freshnessCard(page)
  await expect(card.getByRole('heading', { name: 'Freshness' })).toBeVisible()

  const rows = page.getByRole('listitem')
  await expect(rows).toHaveCount(3)
  await expect(rows.first()).toContainText('mobility.silver.yellow_trips')
  await expect(rows.first()).toContainText('changed')
  await expect(rows.first()).toContainText('42 queries')
})

test('a table nothing wrote in the window says so, in amber', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  const card = freshnessCard(page)
  const orders = card.getByRole('listitem').filter({ hasText: 'orders' })
  await expect(orders).toContainText('No write in 30 days')
  await expect(orders.locator('span.text-warn')).toBeVisible()

  // A write a year old is stale too: amber, but with the sentence that says "changed".
  const kpis = card.getByRole('listitem').filter({ hasText: 'kpis' })
  await expect(kpis).toContainText('changed')
  await expect(kpis.locator('span.text-warn')).toBeVisible()
})

test('a row links to the table in the catalog', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  const first = freshnessCard(page).getByRole('listitem').first()
  await expect(first.getByRole('link')).toHaveAttribute('href', '/catalog?catalog=mobility&ns=silver&tbl=yellow_trips')
})

test('a failing read-tables call shows only its own notice', async ({ page }) => {
  await stubApi(page, { readTablesStatus: 500 })
  await page.goto('/')
  const card = freshnessCard(page)
  await expect(card.getByText('Could not read freshness.')).toBeVisible()
  await expect(card.getByRole('listitem')).toHaveCount(0)

  // The rest of the page is unaffected.
  await expect(page.getByRole('heading', { name: 'The last two weeks' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Catalogs' })).toBeVisible()
  const text = await page.locator('body').innerText()
  expect(text).not.toContain('Invalid Date')
  expect(text).not.toContain('NaN')
})
