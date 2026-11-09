import { test, expect, type Route } from '@playwright/test'

// The Maintenance card (stardelt-nova#598): files, small files, size and snapshots on the
// table page, from Iceberg's `$files` and `$snapshots` metadata tables. A failure of those
// queries hides the card; it never turns the page red.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const FILES = { columns: [], rows: [[14, 14, 190112368]], row_count: 1 }
const SNAPSHOTS = { columns: [], rows: [[35, '2026-11-09 02:04:41.763 UTC']], row_count: 1 }

/** `failHealth` answers 500 for the two metadata queries; everything else still works. */
async function stub(page: import('@playwright/test').Page, failHealth = false) {
  await page.route('**/api/**', (r) => {
    const url = new URL(r.request().url())
    const path = url.pathname

    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analytics'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(r, { catalogs: [{ name: 'warehouse', id: '1', canWrite: false, grants: [] }], default: 'warehouse' })
    }
    if (path === '/api/catalog/namespaces') {
      return json(r, { namespaces: [['sales']] })
    }
    if (path === '/api/catalog/namespaces/sales/tables') {
      return json(r, { identifiers: [{ namespace: ['sales'], name: 'orders' }, { namespace: ['sales'], name: 'report', kind: 'view' }] })
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
    if (path === '/api/query') {
      const sql = r.request().postData() ?? ''
      const metadata = sql.includes('$files') || sql.includes('$snapshots')
      if (metadata && failHealth) return json(r, { error: 'metadata tables are not supported' }, 500)
      if (sql.includes('$files')) return json(r, FILES)
      if (sql.includes('$snapshots')) return json(r, SNAPSHOTS)
      return json(r, { columns: [], rows: [], row_count: 0 })
    }
    return json(r, {})
  })
}

test('a table page shows its files, small files, size and snapshots', async ({ page }) => {
  await stub(page)
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  await expect(page.getByRole('heading', { name: 'Maintenance' })).toBeVisible()
  // `formatBytes` prints binary units, so 190,112,368 bytes is 181 MiB.
  await expect(page.getByText('14 files · 14 smaller than 128 MB · 181 MiB')).toBeVisible()
  await expect(page.getByText('35 snapshots · last committed 2026-11-09 02:04:41.763 UTC')).toBeVisible()
  await expect(page.getByText(/14 files are smaller than 128 MB/)).toBeVisible()
  await expect(page.getByText('Compaction merges them', { exact: false })).toBeVisible()
})

test('a view has no Maintenance card and a failed metadata query leaves the page calm', async ({ page }) => {
  await stub(page, true)
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  // The card is gone, and nothing on the page is red because of it.
  await expect(page.getByRole('heading', { name: 'Maintenance' })).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'orders', level: 1 })).toBeVisible()
  await expect(page.locator('[role="alert"]')).toHaveCount(0)
  await expect(page.locator('.text-bad')).toHaveCount(0)

  // A view's page never shows the card either (its two queries are not made at all). A
  // view's kind is known only when the tree opened it, so walk down the tree.
  await page.goto('/catalog')
  const tree = page.getByRole('list', { name: 'Catalogs' })
  await tree.getByRole('button', { name: 'sales' }).click()
  await tree.getByRole('button', { name: /report/ }).click()
  await expect(page.getByRole('heading', { name: 'report', level: 1 })).toBeVisible()
  await expect(page.getByText('View', { exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Maintenance' })).toHaveCount(0)
})
