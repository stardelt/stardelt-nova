import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test('no selection asks for a table', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    if (path === '/api/catalogs') return json(r, [])
    return json(r, [])
  })
  await page.goto('/lineage')
  await expect(page.getByText('Choose a table')).toBeVisible()
  await page.waitForTimeout(300)
  expect(lineageRequests).toEqual([])
})

test('a graph renders jobs and hides an invisible dataset', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    if (path === '/api/catalogs') return json(r, [])
    if (path === '/api/lineage') {
      return json(r, {
        nodes: [
          { id: 'a', namespace: 'warehouse.sales', name: 'orders', visible: true },
          { id: 'b', namespace: 'warehouse.hr', name: 'salaries', visible: false, label: 'a dataset you cannot see' },
        ],
        edges: [{ from: 'a', to: 'b', jobNamespace: 'jobs', jobName: 'nightly_load', runId: 'r1', eventTime: '2026-01-01T00:00:00Z' }],
        jobs: [{ id: 'p1', kind: 'pipeline', name: 'nightly_load', runs: 3, lastRun: '2026-01-01T00:00:00Z' }],
      })
    }
    return json(r, [])
  })
  await page.goto('/lineage?catalog=warehouse&schema=sales&table=orders')
  await expect(page.getByText('orders', { exact: true })).toBeVisible()
  await expect(page.getByText('a dataset you cannot see', { exact: true })).toBeVisible()
  await expect(page.locator('main').getByText('salaries')).toHaveCount(0)
  await expect(page.locator('svg title', { hasText: 'nightly_load' })).toBeAttached()

  // The job is a pill on the edge, not only a tooltip.
  await expect(page.locator('svg').getByText('nightly_load', { exact: true })).toBeVisible()

  // Hovering a dataset lights its edge and dims the rest.
  await page.locator('svg').getByText('orders', { exact: true }).hover()
  // `toBeVisible` cannot be used here: the two nodes sit on one row, so the lit path is a
  // horizontal line with an empty bounding box. Exactly one edge carries the lit width.
  await expect(page.locator('svg path[stroke-width="2.6"]')).toHaveCount(1)
})

test('the window asks the API for the chosen days', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    if (path === '/api/catalogs') return json(r, [])
    if (path === '/api/lineage') {
      return json(r, {
        nodes: [
          { id: 'a', namespace: 'warehouse.sales', name: 'orders', visible: true },
          { id: 'b', namespace: 'warehouse.sales', name: 'trips', visible: true },
        ],
        edges: [{ from: 'b', to: 'a', jobNamespace: 'jobs', jobName: 'nightly_load', runId: 'r1', eventTime: '2026-01-01T00:00:00Z' }],
        jobs: [],
      })
    }
    return json(r, [])
  })
  await page.goto('/lineage?catalog=warehouse&schema=sales&table=orders&days=7')
  await expect.poll(() => lineageRequests.some((u) => u.includes('days=7'))).toBe(true)
  await page.getByLabel('Lineage window').selectOption('90')
  await expect.poll(() => lineageRequests[lineageRequests.length - 1]).toContain('days=90')
  await expect(page).toHaveURL(/days=90/)
})

test('a forbidden table says so', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    if (path === '/api/catalogs') return json(r, [])
    if (path === '/api/lineage') return json(r, { error: 'forbidden' }, 403)
    return json(r, [])
  })
  await page.goto('/lineage?catalog=warehouse&schema=sales&table=orders')
  await expect(page.locator('main')).toContainText('forbidden')
  await expect(page.locator('main')).not.toContainText('/api/lineage')
})

test('the table header links to its lineage', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(r, { catalogs: [{ name: 'warehouse', id: '1' }], default: 'warehouse' })
    }
    if (path === '/api/catalog/namespaces') return json(r, { namespaces: [] })
    if (path === '/api/catalog/tables/sales/orders') {
      return json(r, {
        'metadata-location': '',
        metadata: { 'table-uuid': 'u', location: '', 'last-updated-ms': 0, schemas: [], 'partition-specs': [] },
      })
    }
    if (path === '/api/catalog/tables/sales/orders/documentation') {
      return json(r, { description: null, tags: [], owner: null, domain: null, columns: [], canEdit: false, editRule: '', canRollback: false, rollbackRule: '' })
    }
    if (path === '/api/catalog/tables/sales/orders/history') {
      return json(r, { snapshots: [], columns: [], rows: [] })
    }
    if (path === '/api/query') {
      return json(r, { columns: [], rows: [], row_count: 0 })
    }
    if (path === '/api/lineage') return json(r, { nodes: [], edges: [] })
    return json(r, {})
  })

  // With ?catalog= in the URL the link carries it...
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')
  // `main` excludes the sidebar, which also has a link named "Lineage".
  const link = page.locator('main').getByRole('link', { name: 'Lineage' })
  await expect(link).toHaveAttribute('href', '/lineage?catalog=warehouse&schema=sales&table=orders')

  // ...and without it, the API's default catalog.
  await page.goto('/catalog?ns=sales&tbl=orders')
  await expect(page.locator('main').getByRole('link', { name: 'Lineage' }))
    .toHaveAttribute('href', '/lineage?catalog=warehouse&schema=sales&table=orders')

  await page.locator('main').getByRole('link', { name: 'Lineage' }).click()
  await expect(page).toHaveURL(/\/lineage\?catalog=warehouse&schema=sales&table=orders/)
  await expect.poll(() => lineageRequests.length).toBeGreaterThan(0)
})

test('the selected table opens in the catalog', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(r, { catalogs: [{ name: 'warehouse', id: '1' }], default: 'warehouse' })
    }
    if (path === '/api/catalog/namespaces') return json(r, { namespaces: [] })
    if (path === '/api/catalog/tables/sales/orders') {
      return json(r, {
        'metadata-location': '',
        metadata: { 'table-uuid': 'u', location: '', 'last-updated-ms': 0, schemas: [], 'partition-specs': [] },
      })
    }
    if (path === '/api/catalog/tables/sales/orders/documentation') {
      return json(r, { description: null, tags: [], owner: null, domain: null, columns: [], canEdit: false, editRule: '', canRollback: false, rollbackRule: '' })
    }
    if (path === '/api/catalog/tables/sales/orders/history') {
      return json(r, { snapshots: [], columns: [], rows: [] })
    }
    if (path === '/api/query') return json(r, { columns: [], rows: [], row_count: 0 })
    if (path === '/api/lineage') {
      return json(r, {
        nodes: [
          { id: 'a', namespace: 'warehouse.sales', name: 'orders', visible: true },
          { id: 'b', namespace: 'warehouse.sales', name: 'trips', visible: true },
        ],
        edges: [{ from: 'b', to: 'a', jobNamespace: 'jobs', jobName: 'nightly_load', runId: 'r1', eventTime: '2026-01-01T00:00:00Z' }],
      })
    }
    return json(r, {})
  })

  await page.goto('/lineage?catalog=warehouse&schema=sales&table=orders')
  const link = page.locator('main').getByRole('link', { name: 'Open in catalog' })
  await expect(link).toHaveAttribute('href', '/catalog?catalog=warehouse&ns=sales&tbl=orders')
  await link.click()
  await expect(page).toHaveURL(/\/catalog\?catalog=warehouse&ns=sales&tbl=orders/)
  await expect(page.locator('main')).toContainText('orders')
})
