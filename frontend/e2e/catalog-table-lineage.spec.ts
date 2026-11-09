import { test, expect, type Route } from '@playwright/test'

// The one-hop lineage card (stardelt-nova#511): the datasets that feed this table and the ones
// it feeds, from `/api/lineage?depth=1`. An invisible node is text, never a link, and a page
// with no catalog open asks for nothing.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const GRAPH = {
  nodes: [
    { id: 'lakehouse.sales|orders', namespace: 'lakehouse.sales', name: 'orders', visible: true },
    { id: 'lakehouse.sales|raw_orders', namespace: 'lakehouse.sales', name: 'raw_orders', visible: true },
    { id: 'lakehouse.gold|revenue', namespace: 'lakehouse.gold', name: 'revenue', visible: true },
    { id: 'lakehouse.hr|salaries', namespace: 'lakehouse.hr', name: 'salaries', visible: false },
  ],
  edges: [
    { from: 'lakehouse.sales|raw_orders', to: 'lakehouse.sales|orders', jobNamespace: 'jobs', jobName: 'load_orders', runId: 'r1', eventTime: '2026-11-09T02:00:00Z' },
    { from: 'lakehouse.sales|orders', to: 'lakehouse.gold|revenue', jobNamespace: 'jobs', jobName: 'build_revenue', runId: 'r2', eventTime: '2026-11-09T03:00:00Z' },
    { from: 'lakehouse.sales|orders', to: 'lakehouse.hr|salaries', jobNamespace: 'jobs', jobName: 'build_salaries', runId: 'r3', eventTime: '2026-11-09T04:00:00Z' },
  ],
}

type Stub = { defaultCatalog?: string }

async function stub(page: import('@playwright/test').Page, { defaultCatalog = 'lakehouse' }: Stub = {}) {
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
    if (path === '/api/lineage') return json(r, GRAPH)
    return json(r, {})
  })
}

test('a table page lists one lineage hop each way and never links an invisible table', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await stub(page)
  await page.goto('/catalog?catalog=lakehouse&ns=sales&tbl=orders')

  // The card reads nothing until its tab is open.
  await expect(page.getByRole('button', { name: 'lineage' })).toBeVisible()
  expect(lineageRequests).toEqual([])

  await page.getByRole('button', { name: 'lineage' }).click()

  await expect(page.getByText('Upstream — what feeds this table')).toBeVisible()
  await expect(page.getByText('Downstream — what reads this table')).toBeVisible()
  await expect(page.getByRole('link', { name: 'lakehouse.sales.raw_orders' })).toBeVisible()
  await expect(page.getByRole('link', { name: 'lakehouse.gold.revenue' })).toBeVisible()
  await expect(page.getByRole('link', { name: 'lakehouse.sales.raw_orders' }))
    .toHaveAttribute('href', '/lineage?schema=sales&table=raw_orders&catalog=lakehouse')
  await expect(page.getByRole('link', { name: 'lakehouse.gold.revenue' }))
    .toHaveAttribute('href', '/lineage?schema=gold&table=revenue&catalog=lakehouse')

  // The invisible dataset is named as text, with the note, and is not a link.
  await expect(page.getByText('lakehouse.hr.salaries · not visible to you')).toBeVisible()
  await expect(page.getByRole('link', { name: /salaries/ })).toHaveCount(0)

  // The full graph stays one click away and carries the table.
  await expect(page.getByRole('link', { name: 'Open the full lineage graph' }))
    .toHaveAttribute('href', '/lineage?schema=sales&table=orders&catalog=lakehouse')

  // It asked the one-hop endpoint the ticket names.
  expect(lineageRequests.some(u => u.includes('depth=1') && u.includes('table=orders'))).toBe(true)
})

test('with no catalog open the card says so and asks for nothing', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await stub(page, { defaultCatalog: '' })
  await page.goto('/catalog?ns=sales&tbl=orders')

  await page.getByRole('button', { name: 'lineage' }).click()
  await expect(page.getByText('No catalog is open, so there is no lineage to read.')).toBeVisible()
  await page.waitForTimeout(300)
  expect(lineageRequests).toEqual([])
})
