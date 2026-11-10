import { test, expect, type Route } from '@playwright/test'

// The table's lineage tab (stardelt-nova#703, lineage L5): who makes this table, what it is made
// from and what uses it. One summary per question, then the two scanning lists — hand-made tables
// folded away, tables the person may not read collapsed into one line, and no query id anywhere.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const GRAPH = {
  nodes: [
    {
      id: 'lakehouse.sales|orders', namespace: 'lakehouse.sales', name: 'orders', visible: true,
      level: 0, lastWrite: '2026-11-09T02:00:00Z',
      readers: { queries: 7, people: 2, lastRead: '2026-11-09T05:00:00Z', tools: ['superset'] },
      writers: [
        { kind: 'pipeline', name: 'shop/load_orders', runs: 30, lastRun: '2026-11-09T02:00:00Z' },
        { kind: 'pipeline', name: 'shop/backfill_orders', runs: 3, lastRun: '2026-11-08T02:00:00Z' },
        { kind: 'query', name: 'alice', runs: 2, lastRun: '2026-11-07T02:00:00Z' },
      ],
    },
    {
      id: 'lakehouse.sales|raw_orders', namespace: 'lakehouse.sales', name: 'raw_orders', visible: true,
      level: -1, lastWrite: '2026-11-09T01:00:00Z', handMade: false,
    },
    {
      id: 'lakehouse.gold|revenue', namespace: 'lakehouse.gold', name: 'revenue', visible: true,
      level: 1, lastWrite: '2026-11-09T03:00:00Z', handMade: false,
    },
    // A table a person's scratch query built: folded away until asked for.
    {
      id: 'lakehouse.sales|scratch', namespace: 'lakehouse.sales', name: 'scratch', visible: true,
      level: 1, lastWrite: '2026-11-09T03:30:00Z', handMade: true,
    },
    {
      id: 'lakehouse.sales|mart', namespace: 'lakehouse.sales', name: 'mart', visible: true,
      level: 2, lastWrite: '2026-11-09T04:00:00Z', handMade: false,
    },
    // A dataset the person may not read carries no level: it is no row, only one muted line.
    { id: 'hidden-1', namespace: '', name: '', visible: false, label: 'a dataset you cannot see', handMade: false, level: null },
  ],
  edges: [
    { from: 'lakehouse.sales|raw_orders', to: 'lakehouse.sales|orders', jobNamespace: 'pipeline', jobName: 'shop/load_orders', runId: 'r1', eventTime: '2026-11-09T02:00:00Z', runs: 30 },
    { from: 'lakehouse.sales|orders', to: 'lakehouse.gold|revenue', jobNamespace: 'pipeline', jobName: 'shop/build_revenue', runId: 'r2', eventTime: '2026-11-09T03:00:00Z', runs: 30 },
    { from: 'lakehouse.sales|orders', to: 'lakehouse.sales|scratch', jobNamespace: 'sql-editor', jobName: 'alice', runId: 'r3', eventTime: '2026-11-09T03:30:00Z', runs: 2 },
    { from: 'lakehouse.gold|revenue', to: 'lakehouse.sales|mart', jobNamespace: 'pipeline', jobName: 'sales/build_mart', runId: 'r4', eventTime: '2026-11-09T04:00:00Z', runs: 5 },
    { from: 'lakehouse.sales|orders', to: 'hidden-1', jobNamespace: 'notebook', jobName: 'bob', runId: 'r5', eventTime: '2026-11-09T04:30:00Z', runs: 1 },
  ],
  jobs: [
    { id: 'pipeline|shop/load_orders', kind: 'pipeline', name: 'shop/load_orders', runs: 30, lastRun: '2026-11-09T02:00:00Z', handMade: false },
    { id: 'pipeline|shop/build_revenue', kind: 'pipeline', name: 'shop/build_revenue', runs: 30, lastRun: '2026-11-09T03:00:00Z', handMade: false },
    { id: 'pipeline|sales/build_mart', kind: 'pipeline', name: 'sales/build_mart', runs: 5, lastRun: '2026-11-09T04:00:00Z', handMade: false },
    { id: 'sql-editor|alice', kind: 'sql-editor', name: 'alice', runs: 2, lastRun: '2026-11-09T03:30:00Z', handMade: true },
    { id: 'notebook|bob', kind: 'notebook', name: 'bob', runs: 1, lastRun: '2026-11-09T04:30:00Z', handMade: true },
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

test('the lineage tab says who makes the table, from what and what uses it', async ({ page }) => {
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

  // "Made by": the pipeline that writes the table, its kind, its runs and when it last ran.
  await expect(page.getByText('Made by', { exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: 'shop/load_orders' }).first())
    .toHaveAttribute('href', '/pipelines/shop/load_orders')
  await expect(page.getByText(/pipeline · 30 runs · last .* ago/)).toBeVisible()
  // A second pipeline is flagged rather than listed away.
  await expect(page.getByText('2 pipelines write this table')).toBeVisible()
  // The queries people ran by hand are one muted line, never a query id.
  await expect(page.getByText(/Also written by hand: 1 query, last .* ago/)).toBeVisible()

  // "Used by": people first, then the tools, then the reads.
  await expect(page.getByText('2 people')).toBeVisible()
  await expect(page.getByText('superset')).toBeVisible()
  await expect(page.getByText('7 queries')).toBeVisible()

  // Two lists, one line per table, the job that connects it as a pill.
  await expect(page.getByText('Made from', { exact: true })).toBeVisible()
  await expect(page.getByText('Used by', { exact: true })).toHaveCount(2)
  await expect(page.getByRole('link', { name: 'sales.raw_orders' }))
    .toHaveAttribute('href', '/lineage?schema=sales&table=raw_orders&catalog=lakehouse')
  await expect(page.getByRole('link', { name: 'gold.revenue' }))
    .toHaveAttribute('href', '/lineage?schema=gold&table=revenue&catalog=lakehouse')
  await expect(page.getByRole('link', { name: 'shop/build_revenue' }))
    .toHaveAttribute('href', '/pipelines/shop/build_revenue')

  // A pipeline in the table's own namespace loses its prefix: `sales/build_mart` reads `build_mart`.
  await expect(page.getByText('2 steps away')).toBeVisible()
  await expect(page.getByRole('link', { name: 'build_mart' }))
    .toHaveAttribute('href', '/pipelines/sales/build_mart')
  await expect(page.getByRole('link', { name: 'sales.mart' })).toBeVisible()

  // No level chip, no "via", no "last write" text: the time is the row's tooltip.
  await expect(page.getByText('L1', { exact: true })).toHaveCount(0)
  await expect(page.getByText('via', { exact: true })).toHaveCount(0)
  await expect(page.getByText(/last write/)).toHaveCount(0)

  // Tables a query made are folded away until asked for.
  await expect(page.getByRole('link', { name: 'sales.scratch' })).toHaveCount(0)
  await page.getByRole('button', { name: '＋ 1 table made by hand' }).click()
  await expect(page.getByRole('link', { name: 'sales.scratch' })).toHaveAttribute(
    'href', '/lineage?schema=sales&table=scratch&catalog=lakehouse',
  )
  await expect(page.getByRole('main').getByText('alice').last()).toBeVisible()
  await page.getByRole('button', { name: 'Hide tables made by hand' }).click()
  await expect(page.getByRole('link', { name: 'sales.scratch' })).toHaveCount(0)

  // A table the person may not read is one muted line, never a link.
  await expect(page.getByText('1 table you cannot see')).toBeVisible()
  await expect(page.getByRole('link', { name: /cannot see/ })).toHaveCount(0)

  // The drawn graph stays one click away, at the depth the page opens it with.
  await expect(page.getByRole('link', { name: 'Open graph' }))
    .toHaveAttribute('href', '/lineage?schema=sales&table=orders&catalog=lakehouse&depth=2')

  // It asked the tab's own window and depth, and no query id is anywhere on the tab.
  expect(lineageRequests.some(u => u.includes('depth=3') && u.includes('days=30') && u.includes('table=orders'))).toBe(true)
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
  await expect(page.getByText('7 queries')).toBeVisible()

  const refetch = page.waitForRequest(r => r.url().includes('/api/lineage') && r.url().includes('days=7'))
  await page.getByLabel('Lineage window').selectOption('7')
  await refetch

  expect(lineageRequests.some(u => u.includes('days=30'))).toBe(true)
  await expect(page.getByText('7 queries')).toBeVisible()
})

test('an empty window says so in the summary and on both lists', async ({ page }) => {
  const focusOnly = {
    nodes: [{ id: 'lakehouse.sales|orders', namespace: 'lakehouse.sales', name: 'orders', visible: true, level: 0, handMade: false }],
    edges: [],
    jobs: [],
  }
  await stub(page, { graph: focusOnly })
  await page.goto('/catalog?catalog=lakehouse&ns=sales&tbl=orders')
  await page.getByRole('tab', { name: 'lineage' }).click()

  await expect(page.getByText('Nothing wrote this table in 30 days')).toBeVisible()
  await expect(page.getByText('No reads in 30 days')).toBeVisible()
  await expect(page.getByText('Nothing feeds this table in 30 days.')).toBeVisible()
  await expect(page.getByText('No table is built from this one in 30 days.')).toBeVisible()
})

test('at 390 px no row overflows its card', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await stub(page)
  await page.goto('/catalog?catalog=lakehouse&ns=sales&tbl=orders')
  await page.getByRole('tab', { name: 'lineage' }).click()
  await page.getByRole('button', { name: '＋ 1 table made by hand' }).click()
  await expect(page.getByRole('link', { name: 'gold.revenue' })).toBeVisible()

  const overflowing = await page.evaluate(() =>
    Array.from(document.querySelectorAll('main .card'))
      .filter(el => el.scrollWidth > el.clientWidth + 1)
      .map(el => el.textContent?.slice(0, 60) ?? ''),
  )
  expect(overflowing).toEqual([])
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
