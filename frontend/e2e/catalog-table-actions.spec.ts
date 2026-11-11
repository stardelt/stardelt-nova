import { test, expect, type Route } from '@playwright/test'

// The table page's header facts and copy actions (stardelt-nova#765): who owns the table, when it
// last changed, and the two snippets that take it into a notebook or a shell.

test.use({ permissions: ['clipboard-read', 'clipboard-write'] })

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const FIELDS = [
  { id: 1, name: 'id', required: true, type: 'long' },
  { id: 2, name: 'note', required: false, type: 'string' },
]

async function stub(page: import('@playwright/test').Page, updatedMs: number) {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname

    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analytics'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(r, { catalogs: [{ name: 'warehouse', id: '1', canWrite: false, grants: [] }], default: 'warehouse' })
    }
    if (path === '/api/catalog/namespaces') return json(r, { namespaces: [['sales']] })
    if (path === '/api/catalog/namespaces/sales/tables') {
      return json(r, { identifiers: [{ namespace: ['sales'], name: 'orders' }] })
    }
    if (path.startsWith('/api/catalog/tables/')) {
      if (path.endsWith('/documentation')) {
        return json(r, { description: null, tags: [], owner: 'analytics', domain: null, columns: [], canEdit: false, editRule: '', canRollback: false, rollbackRule: '' })
      }
      if (path.endsWith('/history')) return json(r, { snapshots: [], columns: [], rows: [] })
      return json(r, {
        'metadata-location': '',
        metadata: {
          'table-uuid': 'u',
          location: 's3://warehouse/sales/orders',
          'last-updated-ms': updatedMs,
          'current-schema-id': 0,
          schemas: [{ 'schema-id': 0, type: 'struct', fields: FIELDS }],
          'partition-specs': [],
        },
      })
    }
    if (path === '/api/query') return json(r, { columns: [], rows: [], row_count: 0 })
    if (path === '/api/lineage') return json(r, { nodes: [], edges: [], jobs: [], impact: { downstream: 0, upstream: 0, downstreamJobs: 0 } })
    return json(r, {})
  })
}

test('the header copies a query for a notebook and for the CLI', async ({ page }) => {
  await stub(page, Date.now() - 3_600_000)
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  const notebook = page.getByRole('button', { name: 'Use in a notebook' })
  const cli = page.getByRole('button', { name: 'Copy for the CLI' })
  await expect(notebook).toBeVisible()
  await expect(cli).toBeVisible()

  await notebook.click()
  await expect(page.getByText('Copied')).toBeVisible()
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    'import stardelt\nstardelt.sql("SELECT * FROM warehouse.sales.orders LIMIT 100")',
  )

  await cli.click()
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    'stardelt sql "SELECT * FROM warehouse.sales.orders LIMIT 100"',
  )

  // The header says whose table it is and how fresh it is.
  await expect(page.getByText('Owner analytics')).toBeVisible()
  await expect(page.getByText('Last changed')).toBeVisible()
})

test('a table with no commit time shows the owner but no date', async ({ page }) => {
  await stub(page, 0)
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  await expect(page.getByText('Owner analytics')).toBeVisible()
  await expect(page.getByText('Last changed')).toHaveCount(0)
})
