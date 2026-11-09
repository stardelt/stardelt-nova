import { test, expect, type Route } from '@playwright/test'

// The schema page and the breadcrumb (stardelt-nova#585): every level of
// catalog › schema › table is a page, each one click back.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const LONG_NS = 'very_long_schema_name_that_has_to_wrap_on_a_phone'
const LONG_TBL = 'a_table_with_a_long_name'

async function stub(page: import('@playwright/test').Page) {
  await page.route('**/api/**', (r) => {
    const url = new URL(r.request().url())
    const path = url.pathname

    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analytics'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(r, {
        catalogs: [{ name: 'warehouse', id: '1', canWrite: false, grants: [] }, { name: 'bronze', id: '2', layer: 'bronze' }],
        default: 'warehouse',
      })
    }
    if (path === '/api/catalog/namespaces') {
      return json(r, { namespaces: [['sales'], ['ops'], [LONG_NS]] })
    }
    if (path === '/api/catalog/namespaces/sales/tables') {
      return json(r, { identifiers: [{ namespace: ['sales'], name: 'orders' }, { namespace: ['sales'], name: 'customers' }] })
    }
    if (path === '/api/catalog/namespaces/ops/tables') {
      return json(r, { identifiers: [{ namespace: ['ops'], name: 'runs' }, { namespace: ['ops'], name: 'report', kind: 'view' }] })
    }
    if (path === `/api/catalog/namespaces/${LONG_NS}/tables`) {
      return json(r, { identifiers: [{ namespace: [LONG_NS], name: LONG_TBL }] })
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
    return json(r, {})
  })
}

test('a schema is a page: the tree highlights it and the right pane lists its tables', async ({ page }) => {
  await stub(page)
  await page.goto('/catalog')

  const tree = page.getByRole('list', { name: 'Catalogs' })
  const pane = page.locator('main section').first()

  // The catalog page: one h1, the catalog's name, and a one-part breadcrumb.
  await expect(page.getByRole('heading', { name: 'warehouse', level: 1 })).toBeVisible()
  expect(await page.locator('h1').count()).toBe(1)
  const crumb = page.getByRole('navigation', { name: 'Breadcrumb' })
  await expect(crumb.getByText('warehouse')).toHaveAttribute('aria-current', 'page')

  // A click on the schema opens the schema page in the URL and highlights the row.
  await tree.getByRole('button', { name: 'sales' }).click()
  await expect(page).toHaveURL(/[?&]catalog=warehouse&ns=sales$/)
  await expect(tree.getByRole('button', { name: 'sales' })).toHaveAttribute('aria-current', 'true')
  await expect(page.getByRole('heading', { name: 'sales', level: 1 })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Tables and views (2)' })).toBeVisible()
  expect(await page.locator('h1').count()).toBe(1)
  await expect(crumb.getByRole('link', { name: 'warehouse' })).toBeVisible()
  await expect(crumb.getByText('sales')).toHaveAttribute('aria-current', 'page')

  // The list carries the kind of each object and the arrow that says it opens.
  await expect(pane.getByRole('button', { name: /orders table/ })).toBeVisible()
  await expect(pane.getByRole('button', { name: /customers table/ })).toBeVisible()
  await expect(pane.getByRole('link', { name: 'Load data' })).toBeVisible()
})

test('a table row opens the table page and the breadcrumb walks back up', async ({ page }) => {
  await stub(page)
  await page.goto('/catalog')

  const pane = page.locator('main section').first()
  const crumb = page.getByRole('navigation', { name: 'Breadcrumb' })

  // Walk down from the catalog page, one history entry per level.
  await page.getByRole('list', { name: 'Catalogs' }).getByRole('button', { name: 'sales' }).click()
  await expect(page.getByRole('heading', { name: 'sales', level: 1 })).toBeVisible()

  await pane.getByRole('button', { name: /orders/ }).click()
  await expect(page).toHaveURL(/[?&]catalog=warehouse&ns=sales&tbl=orders$/)
  await expect(page.getByRole('heading', { name: 'orders', level: 1 })).toBeVisible()
  expect(await page.locator('h1').count()).toBe(1)
  await expect(crumb.getByRole('link', { name: 'warehouse' })).toBeVisible()
  await expect(crumb.getByRole('link', { name: 'sales' })).toBeVisible()
  await expect(crumb.getByText('orders')).toHaveAttribute('aria-current', 'page')

  // Browser Back walks table → schema → catalog.
  await page.goBack()
  await expect(page.getByRole('heading', { name: 'sales', level: 1 })).toBeVisible()
  await page.goBack()
  await expect(page.getByRole('heading', { name: 'warehouse', level: 1 })).toBeVisible()

  // ... and the breadcrumb's catalog link goes back to the catalog page too.
  await page.goto('/catalog?catalog=warehouse&ns=sales&tbl=orders')
  await page.getByRole('navigation', { name: 'Breadcrumb' }).getByRole('link', { name: 'warehouse' }).click()
  await expect(page).toHaveURL(/[?&]catalog=warehouse$/)
  await expect(page.getByRole('heading', { name: 'warehouse', level: 1 })).toBeVisible()
})

test('at 390 px the page does not scroll sideways and the breadcrumb wraps', async ({ page }) => {
  await stub(page)
  await page.setViewportSize({ width: 390, height: 740 })
  await page.goto(`/catalog?catalog=warehouse&ns=${LONG_NS}&tbl=${LONG_TBL}`)

  const crumb = page.getByRole('navigation', { name: 'Breadcrumb' })
  await expect(crumb.getByText(LONG_TBL)).toHaveAttribute('aria-current', 'page')

  const widths = await page.evaluate(() => ({
    doc: document.documentElement.scrollWidth,
    view: document.documentElement.clientWidth,
  }))
  expect(widths.doc, `document scrollWidth ${widths.doc} vs clientWidth ${widths.view}`).toBeLessThanOrEqual(widths.view)

  // Three parts, no horizontal overflow inside the breadcrumb, and more than one line of them.
  const boxes = await crumb.locator('li').evaluateAll((items) =>
    items.map((el) => {
      const r = el.getBoundingClientRect()
      return { x: r.x, y: r.y, width: r.width }
    }),
  )
  expect(boxes.length).toBe(3)
  for (const box of boxes) expect(box.x + box.width).toBeLessThanOrEqual(390)
  expect(boxes[2].y).toBeGreaterThan(boxes[0].y)
})
