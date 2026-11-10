import { test, expect, type Route } from '@playwright/test'

// The table's lineage tab (stardelt-nova#649, lineage L4): a written-by and read-by summary, then
// the upstream and downstream datasets of the chosen window with the job that connects each. An
// invisible node is text, never a link, and a page with no catalog open asks for nothing.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const GRAPH = {
  nodes: [
    {
      id: 'lakehouse.sales|orders', namespace: 'lakehouse.sales', name: 'orders', visible: true,
      level: 0, lastWrite: '2026-11-09T02:00:00Z', readers: { queries: 7, people: 2, lastRead: '2026-11-09T05:00:00Z' },
    },
    {
      id: 'lakehouse.sales|raw_orders', namespace: 'lakehouse.sales', name: 'raw_orders', visible: true,
      level: -1, lastWrite: '2026-11-09T01:00:00Z',
    },
    {
      id: 'lakehouse.gold|revenue', namespace: 'lakehouse.gold', name: 'revenue', visible: true,
      level: 1, lastWrite: '2026-11-09T03:00:00Z',
    },
    // A dataset the person may not read carries no level, so it is in neither list and is no link.
    { id: 'hidden-1', namespace: '', name: '', visible: false, label: 'a dataset you cannot see' },
  ],
  edges: [
    { from: 'lakehouse.sales|raw_orders', to: 'lakehouse.sales|orders', jobNamespace: 'pipeline', jobName: 'shop/load_orders', runId: 'r1', eventTime: '2026-11-09T02:00:00Z' },
    { from: 'lakehouse.sales|orders', to: 'lakehouse.gold|revenue', jobNamespace: 'pipeline', jobName: 'shop/build_revenue', runId: 'r2', eventTime: '2026-11-09T03:00:00Z' },
    { from: 'lakehouse.sales|orders', to: 'hidden-1', jobNamespace: 'notebook', jobName: 'alice', runId: 'r3', eventTime: '2026-11-09T04:00:00Z' },
  ],
  jobs: [
    { id: 'pipeline|shop/load_orders', kind: 'pipeline', name: 'shop/load_orders', runs: 1, lastRun: '2026-11-09T02:00:00Z' },
    { id: 'pipeline|shop/build_revenue', kind: 'pipeline', name: 'shop/build_revenue', runs: 1, lastRun: '2026-11-09T03:00:00Z' },
    { id: 'notebook|alice', kind: 'notebook', name: 'alice', runs: 1, lastRun: '2026-11-09T04:00:00Z' },
  ],
}

type Stub = { defaultCatalog?: string; graph?: unknown }

async function stub(page: import('@playwright/test').Page, { defaultCatalog = 'lakehouse', graph = GRAPH }: Stub = {}) {
  await page.route('**/api/**', (r) => {
    const url = new URL(r.request().url())
    const path = url.pathname

    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analytics'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(r, { catalogs: defaultCatalog ? [{ name: defaultCatalog, id: '1', canWrite: false, grants: [] }] : [], default: defaultCatalog })
    }
    if (path === '/api/catalog/namespaces') return json(r, { namespaces: [['sales']] })
    if (path === '/api/catalog/namespaces/sales/tables') {
      return json(r, { identifiers: [{ namespace: ['sales'], name: 'orders' }] })
    }
    if (path.startsWith('/api/catalog/tables/')) {
      if (path.endsWith('/documentation')) {
        return json(r, { description: null, tags: [], owner: null, domain: null, columns: [], canEdit: false, editRule: '', canRollback: false, rollbackRule: '' })
      }
      if (path.endsWith('/history')) return json(r, { snapshots: [], columns: [], rows: [] })
      return json(r, {
        'metadata-location': '',
        metadata: { 'table-uuid': 'u', location: '', 'last-updated-ms': 0, schemas: [], 'partition-specs': [] },
      })
    }
    if (path === '/api/query') return json(r, { columns: [], rows: [], row_count: 0 })
    if (path === '/api/lineage') return json(r, graph)
    return json(r, {})
  })
}

test('the lineage tab summarises the writer and readers and lists both directions with job pills', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await stub(page)
  await page.goto('/catalog?catalog=lakehouse&ns=sales&tbl=orders')

  // The tab reads nothing until it is open.
  await expect(page.getByRole('tab', { name: 'lineage' })).toBeVisible()
  expect(lineageRequests).toEqual([])

  await page.getByRole('tab', { name: 'lineage' }).click()

  // The summary: the job of the newest edge into this table, and its readers.
  await expect(page.getByText('Written by')).toBeVisible()
  // The same job pill names the writer in the summary and appears on the row it connects.
  await expect(page.getByRole('link', { name: 'shop/load_orders' }).first())
    .toHaveAttribute('href', '/pipelines/shop/load_orders')
  await expect(page.getByText(/last write .*ago/).first()).toBeVisible()
  await expect(page.getByText('Read by 2 people · 7 queries in 30 days')).toBeVisible()

  // Two lists, each row with its level chip, its table and the job that connects it.
  await expect(page.getByText('Upstream — what feeds this table')).toBeVisible()
  await expect(page.getByText('Downstream — what depends on this table')).toBeVisible()
  await expect(page.getByText('L1', { exact: true })).toHaveCount(2)
  await expect(page.getByRole('link', { name: 'lakehouse.sales.raw_orders' }))
    .toHaveAttribute('href', '/lineage?schema=sales&table=raw_orders&catalog=lakehouse')
  await expect(page.getByRole('link', { name: 'lakehouse.gold.revenue' }))
    .toHaveAttribute('href', '/lineage?schema=gold&table=revenue&catalog=lakehouse')
  await expect(page.getByRole('link', { name: 'shop/build_revenue' }))
    .toHaveAttribute('href', '/pipelines/shop/build_revenue')
  await expect(page.getByText('last write', { exact: false })).toHaveCount(3)

  // A notebook job is a person's name, not a link; a dataset the person cannot read is no link.
  await expect(page.getByText('alice', { exact: true })).toBeVisible()
  await expect(page.getByText('a dataset you cannot see · not visible to you')).toHaveCount(0)
  await expect(page.getByRole('link', { name: /salaries|cannot see/ })).toHaveCount(0)

  // The drawn graph stays one click away, at the depth the page opens it with.
  await expect(page.getByRole('link', { name: 'Open graph' }))
    .toHaveAttribute('href', '/lineage?schema=sales&table=orders&catalog=lakehouse&depth=2')

  // It asked the tab's own window and depth, and no query id is anywhere on the tab.
  expect(lineageRequests.some(u => u.includes('depth=5') && u.includes('days=30') && u.includes('table=orders'))).toBe(true)
  await expect(page.locator('main')).not.toContainText('r1')
  await expect(page.locator('main')).not.toContainText('r2')
})

test('choosing 7 days refetches with that window', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await stub(page)
  await page.goto('/catalog?catalog=lakehouse&ns=sales&tbl=orders')
  await page.getByRole('tab', { name: 'lineage' }).click()
  await expect(page.getByText('Read by 2 people · 7 queries in 30 days')).toBeVisible()

  await page.getByLabel('Lineage window').selectOption('7')

  await expect(page.getByText('Read by 2 people · 7 queries in 7 days')).toBeVisible()
  expect(lineageRequests.some(u => u.includes('days=7'))).toBe(true)
})

test('an empty window says so on both lists and in the summary', async ({ page }) => {
  const focusOnly = {
    nodes: [{ id: 'lakehouse.sales|orders', namespace: 'lakehouse.sales', name: 'orders', visible: true, level: 0 }],
    edges: [],
    jobs: [],
  }
  await stub(page, { graph: focusOnly })
  await page.goto('/catalog?catalog=lakehouse&ns=sales&tbl=orders')
  await page.getByRole('tab', { name: 'lineage' }).click()

  await expect(page.getByText('Nothing upstream in 30 days.')).toBeVisible()
  await expect(page.getByText('Nothing depends on this table in 30 days.')).toBeVisible()
  await expect(page.getByText('No reads in 30 days')).toBeVisible()
  await expect(page.getByText('nothing in 30 days')).toBeVisible()
})

test('with no catalog open the card says so and asks for nothing', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await stub(page, { defaultCatalog: '' })
  await page.goto('/catalog?ns=sales&tbl=orders')

  await page.getByRole('tab', { name: 'lineage' }).click()
  await expect(page.getByText('No catalog is open, so there is no lineage to read.')).toBeVisible()
  await page.waitForTimeout(300)
  expect(lineageRequests).toEqual([])
})
