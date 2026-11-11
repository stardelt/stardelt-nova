import { test, expect, type Route } from '@playwright/test'

// The table page header's row count and size (stardelt-nova#511): read from the current snapshot's
// summary in the metadata the page already fetched, printed only when the counters are real.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const FIELDS = [
  { id: 1, name: 'id', required: true, type: 'long' },
  { id: 2, name: 'note', required: false, type: 'string' },
]

type Summary = Record<string, string | number>

async function stub(page: import('@playwright/test').Page, currentSnapshotId: number | null | undefined, summary?: Summary) {
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
        return json(r, { description: null, tags: [], owner: null, domain: null, columns: [], canEdit: false, editRule: '', canRollback: false, rollbackRule: '' })
      }
      if (path.endsWith('/history')) return json(r, { snapshots: [], columns: [], rows: [] })
      return json(r, {
        'metadata-location': '',
        metadata: {
          'table-uuid': 'u',
          location: 's3://warehouse/sales/orders',
          'last-updated-ms': 0,
          'current-schema-id': 0,
          'current-snapshot-id': currentSnapshotId,
          schemas: [{ 'schema-id': 0, type: 'struct', fields: FIELDS }],
          'partition-specs': [],
          snapshots: [
            { 'snapshot-id': 7, 'timestamp-ms': 0, summary },
          ],
        },
      })
    }
    if (path === '/api/query') return json(r, { columns: [], rows: [], row_count: 0 })
    if (path === '/api/lineage') return json(r, { nodes: [], edges: [], jobs: [], impact: { downstream: 0, upstream: 0, downstreamJobs: 0 } })
    return json(r, {})
  })
}

test('the header shows the current snapshot\'s exact rows and size', async ({ page }) => {
  await stub(page, 7, { 'total-records': '1234', 'total-files-size': '2048' })
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  await expect(page.getByText('1,234 rows · 2.0 KiB')).toBeVisible()
})

test('a table with no snapshot shows no row count but keeps its location', async ({ page }) => {
  await stub(page, -1, { 'total-records': '1234', 'total-files-size': '2048' })
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  await expect(page.getByText('s3://warehouse/sales/orders')).toBeVisible()
  // Scoped to the page header: the Preview tab's own footer says "0 rows · preview" (Catalog.tsx:511).
  await expect(page.locator('header').getByText(/\d+ rows?/)).toHaveCount(0)
})

test('counters that are not numbers show no line and no NaN', async ({ page }) => {
  await stub(page, 7, { 'total-records': 'n/a', 'total-files-size': 'n/a' })
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  await expect(page.getByText('s3://warehouse/sales/orders')).toBeVisible()
  await expect(page.getByText(/NaN/)).toHaveCount(0)
  await expect(page.locator('header').getByText(/^\d+ rows?\b/)).toHaveCount(0)
})
