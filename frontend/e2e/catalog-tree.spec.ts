import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test('catalog explorer shows a tree of catalogs, schemas and tables', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const url = new URL(r.request().url())
    const path = url.pathname
    const catalog = url.searchParams.get('catalog')

    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analytics'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(r, { catalogs: [{ name: 'warehouse', id: '1' }, { name: 'bronze', id: '2', layer: 'bronze' }], default: 'warehouse' })
    }
    if (path === '/api/catalog/namespaces') {
      if (catalog === 'warehouse') return json(r, { namespaces: [['sales'], ['ops']] })
      if (catalog === 'bronze') return json(r, { namespaces: [['raw']] })
      return json(r, { namespaces: [] })
    }
    if (path === '/api/catalog/namespaces/sales/tables') {
      return json(r, { identifiers: [{ namespace: ['sales'], name: 'orders' }, { namespace: ['sales'], name: 'customers' }] })
    }
    if (path === '/api/catalog/namespaces/ops/tables') {
      return json(r, { identifiers: [{ namespace: ['ops'], name: 'runs' }, { namespace: ['ops'], name: 'runs_view', kind: 'view' }] })
    }
    if (path === '/api/catalog/namespaces/raw/tables') {
      return json(r, { identifiers: [] })
    }
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
    return json(r, {})
  })

  await page.goto('/catalog')
  // The left pane is navigation only (stardelt-nova#584); the right pane names the same
  // schemas, so every tree lookup is scoped to the catalog list.
  const tree = page.getByRole('list', { name: 'Catalogs' })

  // 1. both catalog names are visible as rows and no <select> with the label `Catalog` exists
  await expect(page.getByRole('button', { name: /warehouse/ })).toBeVisible()
  await expect(page.getByRole('button', { name: /bronze/ })).toBeVisible()
  await expect(page.getByRole('combobox', { name: 'Catalog' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: /warehouse/ }).locator('[data-kind="catalog"]')).toHaveCount(1)
  await expect(tree.getByRole('button', { name: 'sales' }).locator('[data-kind="schema"]')).toHaveCount(1)

  // 2. warehouse is already open (its schemas sales and ops are visible) and bronze is not (raw invisible until clicked)
  await expect(tree.getByRole('button', { name: 'sales' })).toBeVisible()
  await expect(tree.getByRole('button', { name: 'ops' })).toBeVisible()
  await expect(tree.getByRole('button', { name: 'raw' })).toHaveCount(0)

  // 3. clicking bronze shows raw; clicking sales shows orders and customers
  await page.getByRole('button', { name: /bronze/ }).click()
  await expect(tree.getByRole('button', { name: 'raw' })).toBeVisible()
  await tree.getByRole('button', { name: 'sales' }).click()
  await expect(tree.getByRole('button', { name: /orders/ })).toBeVisible()
  await expect(tree.getByRole('button', { name: /customers/ })).toBeVisible()
  await tree.getByRole('button', { name: 'ops' }).click()
  await expect(tree.getByRole('button', { name: /runs/ }).locator('[data-kind="table"]')).toHaveCount(1)
  await expect(tree.getByRole('button', { name: /runs_view/ }).locator('[data-kind="view"]')).toHaveCount(1)

  // 4. clicking orders shows the detail heading orders and marks the row aria-current
  await tree.getByRole('button', { name: /orders/ }).click()
  await expect(page.getByRole('heading', { name: 'orders' })).toBeVisible()
  await expect(tree.getByRole('button', { name: /orders/ })).toHaveAttribute('aria-current', 'true')

  // 5. typing runs in the Filter box hides sales (no match) and keeps ops with runs
  await page.getByLabel('Filter tables').fill('runs')
  await expect(tree.getByRole('button', { name: 'sales' })).toHaveCount(0)
  await expect(tree.getByRole('button', { name: 'ops' })).toBeVisible()
  await expect(tree.getByRole('button', { name: 'runs', exact: true })).toBeVisible()

  await tree.getByRole('button', { name: /runs_view/ }).click()
  await expect(page.getByRole('heading', { name: 'runs_view' })).toBeVisible()
  // A view keeps the preview and no table-only tabs.
  await expect(page.getByRole('button', { name: 'columns' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'details' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'history' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'preview' })).toBeVisible()
  await expect(page.getByText('View', { exact: true })).toBeVisible()
})
