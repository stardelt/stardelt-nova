import { test, expect, type Route } from '@playwright/test'

// The Profile tab on a table page (stardelt-nova#511): null share, distinct values and range
// per column, from one `/api/query` statement over the first 10,000 rows. A failed statement
// leaves one calm error box with Retry and nothing else red on the page.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const FIELDS = [
  { id: 1, name: 'id', required: true, type: 'long' },
  { id: 2, name: 'note', required: false, type: 'string' },
]

// count(*) = 1000; per column four items, in schema order `id` then `note`:
// id: 900 non-null, 42 distinct, min 1, max 1000 — note: 400 non-null, 250 distinct, alpha..zeta.
const PROFILE = { columns: [], rows: [[1000, 900, 42, 1, 1000, 400, 250, 'alpha', 'zeta']], row_count: 1 }

/** `failProfile` answers 500 for the profile statement; everything else still works. */
async function stub(page: import('@playwright/test').Page, failProfile = false) {
  await page.route('**/api/**', async (r) => {
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
        metadata: {
          'table-uuid': 'u',
          location: '',
          'last-updated-ms': 0,
          'current-schema-id': 0,
          schemas: [{ 'schema-id': 0, type: 'struct', fields: FIELDS }],
          'partition-specs': [],
        },
      })
    }
    if (path === '/api/query') {
      const sql = r.request().postData() ?? ''
      if (sql.includes('LIMIT 10000')) {
        if (failProfile) return json(r, { error: 'profiling is not supported' }, 500)
        return json(r, PROFILE)
      }
      return json(r, { columns: [], rows: [], row_count: 0 })
    }
    return json(r, {})
  })
}

test('a table page profiles its columns over the first 10,000 rows', async ({ page }) => {
  await stub(page)
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  // The tab sits between Preview and Columns, and only a table has it.
  const profileTab = page.getByRole('button', { name: 'profile' })
  await expect(profileTab).toBeVisible()

  await profileTab.click()
  await expect(page.getByText('Profiled from the first 10,000 rows', { exact: false })).toBeVisible()
  // 1000 rows, 900 non-null in `id`, so 10.0% are null.
  await expect(page.getByText('10.0% null')).toBeVisible()
  await expect(page.getByText('42')).toBeVisible()
  await expect(page.getByText('alpha … zeta')).toBeVisible()

  // A view has no Profile tab. Its kind is known only when the tree opened it, so walk down.
  await page.goto('/catalog')
  const tree = page.getByRole('list', { name: 'Catalogs' })
  await tree.getByRole('button', { name: 'sales' }).click()
  await tree.getByRole('button', { name: /report/ }).click()
  await expect(page.getByRole('heading', { name: 'report', level: 1 })).toBeVisible()
  await expect(page.getByRole('button', { name: 'profile' })).toHaveCount(0)
})

test('a failed profile statement is one calm error box with Retry', async ({ page }) => {
  await stub(page, true)
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')
  await page.getByRole('button', { name: 'profile' }).click()

  const alert = page.locator('[role="alert"]')
  await expect(alert).toHaveCount(1)
  await expect(alert.getByRole('button', { name: 'Retry' })).toBeVisible()
  await expect(page.getByText('profiling is not supported')).toBeVisible()
})
