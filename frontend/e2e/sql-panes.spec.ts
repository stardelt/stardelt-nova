import { test, expect, type Page, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

/**
 * The SQL editor's two side panes (stardelt-nova#586): the catalog tree on the left, the
 * history on the right, both collapsible and remembered, and a table click that inserts
 * `catalog.schema.table` at the cursor.
 */
async function open(page: Page) {
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname

    if (path === '/api/me') {
      return json(route, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analysts'] })
    }
    if (path === '/api/catalog/catalogs') {
      return json(route, { catalogs: [{ name: 'warehouse', id: '1' }, { name: 'finance', id: '2' }], default: 'warehouse' })
    }
    if (path === '/api/catalog/namespaces') {
      const catalog = url.searchParams.get('catalog')
      if (catalog === 'finance') return json(route, { namespaces: [['seed']] })
      return json(route, { namespaces: [['nyc_taxi']] })
    }
    if (path === '/api/catalog/namespaces/nyc_taxi/tables') {
      return json(route, { identifiers: [{ namespace: ['nyc_taxi'], name: 'trips' }] })
    }
    if (path === '/api/catalog/namespaces/seed/tables') {
      return json(route, { identifiers: [{ namespace: ['seed'], name: 'nation' }] })
    }
    if (path === '/api/catalog/tables/nyc_taxi/trips') {
      return json(route, {
        'metadata-location': 'x',
        metadata: {
          'table-uuid': 'u',
          location: 'l',
          'last-updated-ms': 0,
          'current-schema-id': 0,
          schemas: [
            {
              'schema-id': 0,
              fields: [
                { id: 1, name: 'trip_id', required: true, type: 'long' },
                { id: 2, name: 'fare', required: false, type: 'double' },
              ],
            },
          ],
          'partition-specs': [],
        },
      })
    }
    if (path === '/api/saved-queries') return json(route, [])
    if (path === '/api/warehouses') return json(route, [])
    if (path === '/api/query') return json(route, { columns: [], rows: [], row_count: 0, query_id: 'q1' })
    return json(route, {})
  })
}

test('the Tables pane inserts a table name at the cursor, and the catalog still scopes the sample', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await open(page)
  await page.goto('/query')

  // The dropdown is gone; the tree is the way to pick a catalog and a table.
  await expect(page.getByRole('combobox', { name: 'Catalog' })).toHaveCount(0)
  const tree = page.getByRole('list', { name: 'Catalogs' })
  await expect(tree.getByRole('button', { name: /warehouse/ })).toBeVisible()

  const editor = page.getByLabel('SQL query')
  await editor.fill('SELECT * FROM ')
  await editor.click()
  // The caret is right after the space the person typed.
  await page.keyboard.press('End')

  await tree.getByRole('button', { name: 'nyc_taxi' }).click()
  await tree.getByRole('button', { name: 'trips', exact: true }).click()
  await expect(editor).toBeFocused()
  await expect(editor).toHaveValue('SELECT * FROM warehouse.nyc_taxi.trips')
  // The caret landed after the inserted name, so typing continues the statement.
  await page.keyboard.type(' LIMIT 5')
  await expect(editor).toHaveValue('SELECT * FROM warehouse.nyc_taxi.trips LIMIT 5')
  // A pick leaves the URL alone: the statement, not the selection, is what is kept.
  await expect(page).not.toHaveURL(/[?&](ns|tbl)=/)

  // The ⊕ on the row does the same thing from the hover affordance.
  await tree.getByRole('button', { name: 'Insert nyc_taxi.trips' }).click()
  await expect(editor).toBeFocused()
  await expect(editor).toHaveValue('SELECT * FROM warehouse.nyc_taxi.trips LIMIT 5warehouse.nyc_taxi.trips')

  // A catalog click scopes the sample, exactly as the dropdown did.
  await tree.getByRole('button', { name: /finance/ }).click()
  await expect(page).toHaveURL(/[?&]catalog=finance\b/)
  await page.getByRole('button', { name: 'Sample' }).click()
  await expect(editor).toHaveValue('SELECT * FROM finance.seed.nation LIMIT 100')
})

test('the disclosure lists a table’s columns and a column inserts only itself', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const tableReads: string[] = []
  page.on('request', (req) => {
    if (new URL(req.url()).pathname.startsWith('/api/catalog/tables/')) tableReads.push(req.url())
  })
  await open(page)
  await page.goto('/query')

  const tree = page.getByRole('list', { name: 'Catalogs' })
  await tree.getByRole('button', { name: 'nyc_taxi' }).click()
  await expect(tree.getByRole('button', { name: 'trips', exact: true })).toBeVisible()

  // Nothing about the columns is read, and nothing is listed, until the disclosure is opened.
  expect(tableReads).toEqual([])
  await expect(tree.getByRole('button', { name: 'trip_id' })).toHaveCount(0)
  await expect(tree.getByRole('button', { name: 'fare' })).toHaveCount(0)

  await tree.getByRole('button', { name: 'Show the columns of nyc_taxi.trips' }).click()
  await expect(tree.getByRole('button', { name: 'trip_id' })).toBeVisible()
  await expect(tree.getByRole('button', { name: 'fare' })).toBeVisible()
  expect(tableReads).toHaveLength(1)

  const editor = page.getByLabel('SQL query')
  await editor.fill('')
  await editor.click()
  await page.keyboard.press('End')
  await tree.getByRole('button', { name: 'fare' }).click()
  // A column is just the column — not `warehouse.nyc_taxi.trips`.
  await expect(editor).toBeFocused()
  await expect(editor).toHaveValue('fare')
})

test('a collapsed pane widens the editor and the choice survives a reload', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await open(page)
  await page.goto('/query')

  const editor = page.getByLabel('SQL query')
  const openWidth = (await editor.boundingBox())!.width

  await page.getByRole('button', { name: 'Collapse Tables' }).click()
  await expect(page.getByRole('button', { name: 'Show Tables' })).toHaveAttribute('aria-expanded', 'false')
  const collapsedWidth = (await editor.boundingBox())!.width
  expect(collapsedWidth).toBeGreaterThan(openWidth + 100)

  await page.reload()
  await expect(page.getByRole('button', { name: 'Show Tables' })).toBeVisible()
  expect((await page.getByLabel('SQL query').boundingBox())!.width).toBeGreaterThan(openWidth + 100)

  // The keyboard reaches the control and Enter toggles it.
  await page.getByRole('button', { name: 'Show Tables' }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('button', { name: 'Collapse Tables' })).toHaveAttribute('aria-expanded', 'true')
  expect((await page.getByLabel('SQL query').boundingBox())!.width).toBeLessThan(collapsedWidth)

  await page.getByRole('button', { name: 'Collapse History' }).click()
  await page.reload()
  await expect(page.getByRole('button', { name: 'Show History' })).toBeVisible()
})

test('at 390 px both panes start collapsed and the page does not scroll sideways', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 780 })
  await open(page)
  await page.goto('/query')

  const tables = page.getByRole('button', { name: 'Show Tables' })
  const history = page.getByRole('button', { name: 'Show History' })
  await expect(tables).toBeVisible()
  await expect(history).toBeVisible()
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  expect(overflow).toBeLessThanOrEqual(1)

  // Opened, the Tables pane sits above the editor.
  await tables.click()
  const pane = page.getByRole('button', { name: 'Collapse Tables' })
  const editor = page.getByLabel('SQL query')
  const paneBox = (await pane.boundingBox())!
  const editorBox = (await editor.boundingBox())!
  expect(paneBox.y).toBeLessThan(editorBox.y)
  const after = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  expect(after).toBeLessThanOrEqual(1)
})
