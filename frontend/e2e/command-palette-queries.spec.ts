import { test, expect, type Route } from '@playwright/test'

// The ⌘K palette finds saved queries and opens one in the SQL editor
// (stardelt-nova#607, slice b2 of #509). Every `/api/**` call is stubbed, so the
// test needs no cluster.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice',
  auth_mode: 'oidc',
  demo: false,
  warehouse: 'warehouse',
  is_admin: false,
  groups: ['analytics'],
}

const CATALOGS = { catalogs: [{ name: 'lakehouse' }] }

const SAVED = [
  { id: 1, title: 'trip revenue', sql: 'SELECT 1', warehouse: 'warehouse', created_at: 0, updated_at: 0, owner_sub: 'alice', shared_with: [] },
]

const QUERIES = {
  items: [
    {
      id: 'q1',
      state: 'FINISHED',
      user: 'alice',
      source: 'sql-editor',
      warehouse: 'default',
      queryType: 'SELECT',
      sql: 'select * from mobility.gold.kpis',
      createdAt: 1762700000000,
      wallMs: 12,
      outputRows: 3,
      errorCode: null,
    },
  ],
  nextCursor: null,
}

const QUERY_DETAIL = {
  id: 'q1',
  state: 'FINISHED',
  user: 'alice',
  source: 'sql-editor',
  clientTags: [],
  warehouse: 'default',
  catalog: 'mobility',
  schema: 'gold',
  queryType: 'SELECT',
  sql: 'select * from mobility.gold.kpis',
  sqlHash: 'abc',
  createdAt: 1762700000000,
  startedAt: 1762700000000,
  endedAt: 1762700000012,
  timing: { queuedMs: 0, planningMs: 2, executionMs: 10, wallMs: 12, cpuMs: 5 },
  data: {
    inputRows: 3,
    inputBytes: 24,
    outputRows: 3,
    outputBytes: 24,
    writtenRows: null,
    writtenBytes: null,
    peakMemoryBytes: 1024,
  },
  tablesRead: ['mobility.gold.kpis'],
  tablesWritten: [],
  error: null,
}

/** Every API call the shell and the SQL editor make, plus the saved queries the palette reads. */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/settings') return json(route, {})
    if (path.startsWith('/api/settings/')) return json(route, { key: path.split('/').pop(), value: null })
    if (path === '/api/catalog/catalogs') return json(route, CATALOGS)
    if (path === '/api/catalog/namespaces') return json(route, { namespaces: [] })
    if (path === '/api/apps/links') return json(route, [])
    // The SQL editor's WarehouseNote reads this as an array (`sql-panes.spec.ts:34`); the
    // catch-all `{}` made the route throw in CI and the page render as an error (#614).
    if (path === '/api/warehouses') return json(route, [])
    if (path === '/api/saved-queries') return json(route, SAVED)
    if (path === '/api/query-log') return json(route, QUERIES)
    if (path === '/api/query-log/q1') return json(route, QUERY_DETAIL)
    return json(route, {})
  })
}

test('the palette finds a saved query and opens it in the SQL editor', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  const input = page.getByRole('combobox', { name: 'Search pages, queries and tables' })

  // An empty query shows no saved-query rows, even though the list is loaded.
  await expect(dialog.getByText('Saved queries')).toHaveCount(0)
  await expect(dialog.getByRole('option', { name: /trip revenue/ })).toHaveCount(0)

  // A part of the title matches.
  await input.fill('trip')
  await expect(dialog.getByText('Saved queries')).toBeVisible()
  await expect(dialog.getByRole('option', { name: /trip revenue/ })).toBeVisible()

  // A part of the SQL matches too.
  await input.fill('select')
  await expect(dialog.getByRole('option', { name: /trip revenue/ })).toBeVisible()

  await input.fill('trip')
  await dialog.getByRole('option', { name: /trip revenue/ }).click()
  await expect(page).toHaveURL(/\/query\?sql=SELECT%201$/)
  await expect(page.getByRole('textbox', { name: 'SQL query' })).toHaveValue('SELECT 1')
  await expect(dialog).toHaveCount(0)
})

test('the palette finds a recent query and opens it in the query log', async ({ page }) => {
  let detailHits = 0
  page.on('request', request => {
    if (new URL(request.url()).pathname === '/api/query-log/q1') detailHits++
  })
  await stubApi(page)
  await page.goto('/')
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  const input = page.getByRole('combobox', { name: 'Search pages, queries and tables' })

  // Nothing is asked of the query log for fewer than two characters.
  await expect(dialog.getByText('Queries', { exact: true })).toHaveCount(0)

  await input.fill('kpis')
  await expect(dialog.getByText('Queries', { exact: true })).toBeVisible()
  const row = dialog.getByRole('option', { name: /select \* from mobility\.gold\.kpis/ })
  await expect(row).toBeVisible()

  await row.click()
  await expect(page).toHaveURL(/\/queries\?id=q1$/)
  await expect(dialog).toHaveCount(0)
  // The page opens that query's details: the detail route was asked for it.
  await expect(page.locator('pre').filter({ hasText: 'select * from mobility.gold.kpis' })).toBeVisible()
  expect(detailHits).toBeGreaterThan(0)

  // The address survives a reload, so a link to one query can be shared.
  await page.reload()
  await expect(page).toHaveURL(/\/queries\?id=q1$/)
  await expect(page.locator('pre').filter({ hasText: 'select * from mobility.gold.kpis' })).toBeVisible()
})
