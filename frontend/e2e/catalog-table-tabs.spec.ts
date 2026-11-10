import { test, expect, type Route } from '@playwright/test'

// The table page's view tabs (stardelt-nova#705): one labelled tab strip whose buttons wrap on a
// phone-width screen, so every view is reachable without a sideways swipe nobody can see.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const FIELDS = [
  { id: 1, name: 'id', required: true, type: 'long' },
  { id: 2, name: 'note', required: false, type: 'string' },
]

async function stub(page: import('@playwright/test').Page) {
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

test('the table views are one labelled tab strip', async ({ page }) => {
  await stub(page)
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  const strip = page.getByRole('tablist', { name: 'Table views' })
  await expect(strip).toBeVisible()

  // Six views, each a tab, and the open one is selected rather than pressed.
  await expect(strip.getByRole('tab')).toHaveCount(6)
  const preview = page.getByRole('tab', { name: 'Preview' })
  await expect(preview).toHaveAttribute('aria-selected', 'true')
  expect(await preview.getAttribute('aria-pressed')).toBeNull()

  await page.getByRole('tab', { name: 'History' }).click()
  await expect(page.getByRole('tab', { name: 'History' })).toHaveAttribute('aria-selected', 'true')
  await expect(preview).toHaveAttribute('aria-selected', 'false')
  expect(await page.getByRole('tab', { name: 'History' }).getAttribute('aria-pressed')).toBeNull()
})

test('on a 390 px screen the tabs wrap and every one is on the screen', async ({ page }) => {
  await stub(page)
  await page.setViewportSize({ width: 390, height: 800 })
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')

  // All six are there, including the last one the old single row pushed off the screen.
  const names = ['Preview', 'Profile', 'Columns', 'Details', 'History', 'Lineage']
  for (const name of names) {
    await expect(page.getByRole('tab', { name })).toBeVisible()
  }
  await expect(page.getByRole('tab', { name: 'Lineage' })).toBeVisible()

  // Every tab fits the viewport, and the strip itself does not scroll sideways.
  for (const name of names) {
    const box = await page.getByRole('tab', { name }).boundingBox()
    expect(box, name).not.toBeNull()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(390)
  }
  const strip = page.getByRole('tablist', { name: 'Table views' })
  const fits = await strip.evaluate((el) => el.scrollWidth <= el.clientWidth)
  expect(fits).toBe(true)

  // They wrap: the last tab starts on a later row than the first one.
  const first = await page.getByRole('tab', { name: 'Preview' }).boundingBox()
  const last = await page.getByRole('tab', { name: 'Lineage' }).boundingBox()
  expect(last!.y).toBeGreaterThan(first!.y)
})
