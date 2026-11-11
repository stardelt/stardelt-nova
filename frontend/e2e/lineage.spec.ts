import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

/**
 * The lineage graph page (stardelt-nova#704). The answer of the backend ticket #702: a source
 * one step before the focus, two consumers one step after it, one consumer the person may not
 * read and one table a query made by hand.
 */
const LAST = '2026-01-01T00:00:00Z'

const graph = () => ({
  nodes: [
    { id: 'raw', namespace: 'lakehouse.landing', name: 'trips_raw', visible: true, level: -1, handMade: false },
    { id: 'clean', namespace: 'lakehouse.staging', name: 'trips_clean', visible: true, level: 0, handMade: false, lastWrite: LAST,
      readers: { queries: 12, people: 3, lastRead: LAST, tools: ['superset'] },
      writers: [{ kind: 'pipeline', name: 'taxi/load-trips', runs: 30, lastRun: LAST }] },
    { id: 'revenue', namespace: 'lakehouse.marts', name: 'daily_revenue', visible: true, level: 1, handMade: false, lastWrite: LAST },
    { id: 'hidden', namespace: 'secret.marts', name: 'hidden_table', visible: false, level: 1, handMade: false },
    { id: 'scratch', namespace: 'lakehouse.staging', name: 'scratch', visible: true, level: 1, handMade: true, lastWrite: LAST },
  ],
  edges: [
    { from: 'raw', to: 'clean', jobNamespace: 'pipeline', jobName: 'taxi/load-trips', runId: 'r1', eventTime: LAST, runs: 30 },
    { from: 'clean', to: 'revenue', jobNamespace: 'pipeline', jobName: 'marts/revenue', runId: 'r2', eventTime: LAST, runs: 12 },
    { from: 'clean', to: 'hidden', jobNamespace: 'pipeline', jobName: 'marts/revenue', runId: 'r3', eventTime: LAST, runs: 4 },
    { from: 'clean', to: 'scratch', jobNamespace: 'sql-editor', jobName: 'alice', runId: 'r4', eventTime: LAST, runs: 2 },
  ],
  jobs: [
    { id: 'pipeline|taxi/load-trips', kind: 'pipeline', name: 'taxi/load-trips', runs: 30, lastRun: LAST, handMade: false },
    { id: 'pipeline|marts/revenue', kind: 'pipeline', name: 'marts/revenue', runs: 16, lastRun: LAST, handMade: false },
    { id: 'sql-editor|alice', kind: 'sql-editor', name: 'alice', runs: 2, lastRun: LAST, handMade: true },
  ],
  // Two visible tables downstream (a hidden one is not counted), one upstream.
  impact: { downstream: 2, upstream: 1, downstreamJobs: 2 },
})

const me = { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] }

/** Every request of the page: `/api/lineage` answers the fixture, everything else is empty. */
async function stub(page: import('@playwright/test').Page, lineage: unknown = graph(), runs: unknown = []) {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/catalogs') return json(r, [])
    if (path === '/api/lineage') return json(r, lineage)
    if (path === '/api/runs') return json(r, runs)
    return json(r, [])
  })
}

test('no selection asks for a table', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await stub(page)
  await page.goto('/lineage')
  await expect(page.getByText('Pick a table to see what it is made from')).toBeVisible()
  await page.waitForTimeout(300)
  expect(lineageRequests).toEqual([])
})

test('the graph puts sources left and consumers right, and labels only on hover', async ({ page }) => {
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  const node = (name: string) => page.locator('main').getByRole('button', { name: new RegExp(name) })
  await expect(node('trips_clean')).toBeVisible()
  await expect(node('trips_raw')).toBeVisible()
  await expect(node('daily_revenue')).toBeVisible()
  // A table the person may not read is drawn, but not named.
  await expect(page.getByText('A table you cannot see')).toBeVisible()
  await expect(page.locator('main').getByText('hidden_table')).toHaveCount(0)

  // Columns come from the API's level: source, focus, consumers.
  const box = async (name: string) => (await node(name).boundingBox())!
  expect((await box('trips_raw')).x).toBeLessThan((await box('trips_clean')).x)
  expect((await box('trips_clean')).x).toBeLessThan((await box('daily_revenue')).x)
  // One row per table, the focus included: nothing is drawn twice.
  expect(await page.locator('.react-flow__node').count()).toBe(4)

  // The graph fits itself when the answer arrives; a pointer that arrives while it still
  // moves misses the table it aimed at.
  await expect.poll(async () => {
    const before = await box('trips_clean')
    await page.waitForTimeout(80)
    const after = await box('trips_clean')
    return Math.abs(before.x - after.x) + Math.abs(before.y - after.y)
  }).toBeLessThan(1)

  // The job name is not on the drawing until a node is looked at. (The toolbar's
  // "Made by" line names the focus table's writer, which is not an edge label.)
  const drawing = page.locator('.react-flow')
  await expect(drawing.getByText('taxi/load-trips')).toHaveCount(0)
  // A browser reports a mouse enter when the pointer changes the element under it. The
  // enter onto a table that is still settling can be followed by a leave in the same frame,
  // so the pointer leaves the drawing and comes back until the table holds the labels.
  const focus = await box('trips_clean')
  await expect.poll(async () => {
    await page.mouse.move(2, 2)
    await page.waitForTimeout(60)
    await page.mouse.move(focus.x + focus.width / 2, focus.y + focus.height / 2)
    await page.waitForTimeout(150)
    return drawing.getByText('taxi/load-trips').count()
  }).toBeGreaterThan(0)
  await expect(drawing.getByText('marts/revenue').first()).toBeVisible()
})

test('a table made by hand is folded away until the checkbox is set', async ({ page }) => {
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')
  const byHand = page.getByRole('checkbox', { name: /Show tables made by hand \(1\)/ })
  await expect(byHand).not.toBeChecked()
  await expect(page.locator('main').getByRole('button', { name: /scratch/ })).toHaveCount(0)

  // The checkbox follows the URL, and the router's navigation lands a tick later.
  await byHand.click()
  await expect(byHand).toBeChecked()
  await expect(page).toHaveURL(/handmade=1/)
  await expect(page.locator('main').getByRole('button', { name: /scratch/ })).toBeVisible()
})

test('a column of more than eight tables folds into one more row', async ({ page }) => {
  const big = graph()
  for (let i = 0; i < 10; i++) {
    big.nodes.push({
      id: `consumer${i}`,
      namespace: 'lakehouse.marts',
      name: `consumer_${i}`,
      visible: true,
      level: 1,
      handMade: false,
      lastWrite: LAST,
    } as (typeof big.nodes)[number])
    big.edges.push({
      from: 'clean',
      to: `consumer${i}`,
      jobNamespace: 'pipeline',
      jobName: `marts/consumer-${i}`,
      runId: `c${i}`,
      eventTime: LAST,
      runs: 3,
    })
  }
  await stub(page, big)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  // Seven kept, one folded row, the source and the focus: ten nodes of thirteen tables.
  await expect(page.getByRole('button', { name: /＋ 5 more/ })).toBeVisible()
  expect(await page.locator('.react-flow__node').count()).toBe(10)

  await page.getByRole('button', { name: /＋ 5 more/ }).click()
  await expect(page.getByRole('button', { name: /＋ 5 more/ })).toHaveCount(0)
  await expect(page.locator(".react-flow__node")).toHaveCount(14)
})

test('a clicked table opens the panel and Escape closes it', async ({ page }) => {
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  await page.locator('main').getByRole('button', { name: /trips_clean/ }).click()
  const panel = page.locator('main').getByRole('complementary', { name: 'trips_clean lineage' })
  await expect(panel).toBeVisible()
  await expect(panel).toContainText('Made by')
  await expect(panel.getByText('taxi/load-trips')).toBeVisible()
  await expect(panel).toContainText('3 people')
  await expect(panel).toContainText('12 queries')
  await expect(panel.getByRole('link', { name: 'Open table' }))
    .toHaveAttribute('href', '/catalog?catalog=lakehouse&ns=staging&tbl=trips_clean')

  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
})

test('Center here makes the table the focus', async ({ page }) => {
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  await page.locator('main').getByRole('button', { name: /daily_revenue/ }).click()
  await page.locator('main').getByRole('button', { name: 'Center here' }).click()
  await expect(page).toHaveURL(/catalog=lakehouse/)
  await expect(page).toHaveURL(/schema=marts/)
  await expect(page).toHaveURL(/table=daily_revenue/)
})

test('the steps control asks the API for that depth', async ({ page }) => {
  const requests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) requests.push(r.url())
  })
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')
  await expect.poll(() => requests.some((u) => u.includes('depth=2'))).toBe(true)

  await page.getByRole('button', { name: '3', exact: true }).click()
  await expect.poll(() => requests[requests.length - 1]).toContain('depth=3')
  await expect(page).toHaveURL(/depth=3/)
})

test('the window asks the API for the chosen days', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await stub(page)
  await page.goto('/lineage?catalog=warehouse&schema=sales&table=orders&days=7')
  await expect.poll(() => lineageRequests.some((u) => u.includes('days=7'))).toBe(true)
  await page.getByLabel('Lineage window').selectOption('90')
  await expect.poll(() => lineageRequests[lineageRequests.length - 1]).toContain('days=90')
  await expect(page).toHaveURL(/days=90/)
})

test('a chain longer than the drawn steps says how many tables there are', async ({ page }) => {
  // The answer of a depth=1 request: the focus and its two one-step consumers. A third table
  // sits two steps away, out of the drawing, and `impact` counts it (nova#748).
  const deep = graph()
  deep.impact = { downstream: 3, upstream: 1, downstreamJobs: 3 }

  await page.setViewportSize({ width: 390, height: 844 })
  await stub(page, deep)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean&depth=1')

  const note = page.getByTestId('lineage-impact-downstream')
  await expect(note).toHaveText('3 tables at every step · 2 drawn')
  // Only the "Used by" list says it; the upstream number is not part of this slice.
  await expect(page.locator('[data-testid^="lineage-impact"]')).toHaveCount(1)

  // Nothing is said when the drawing already shows every table (the fixture's own answer).
  await stub(page, graph())
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean&depth=2')
  await expect(page.getByText('Used by')).toBeVisible()
  await expect(page.getByTestId('lineage-impact-downstream')).toHaveCount(0)
})

test('a phone gets the two lists instead of the graph', async ({ page }) => {  await page.setViewportSize({ width: 390, height: 844 })
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  await expect(page.getByText('The graph needs a wider screen.')).toBeVisible()
  await expect(page.getByText('Made from')).toBeVisible()
  await expect(page.getByText('Used by')).toBeVisible()
  await expect(page.locator('.react-flow')).toHaveCount(0)
  // Rule U4: the page never scrolls sideways.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  expect(overflow).toBeLessThanOrEqual(1)
})

test('a forbidden table says so', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, me)
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
    if (path === '/api/me') return json(r, me)
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

test('a table carries the state of the run that last wrote it', async ({ page }) => {
  const runs = {
    runs: [{ namespace: 'taxi', name: 'load-trips', state: 'failed', startedAt: LAST }],
  }
  await stub(page, graph(), runs)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  // `trips_clean` is written by `taxi/load-trips`, which failed.
  const dot = page.locator('main').getByRole('button', { name: /trips_clean/ }).getByTestId('node-health')
  await expect(dot).toHaveCount(1)
  await expect(dot).toHaveAttribute('aria-label', /^Last run: failed/)
  // `daily_revenue` is written by `marts/revenue`, which has no run in the list: no dot.
  await expect(
    page.locator('main').getByRole('button', { name: /daily_revenue/ }).getByTestId('node-health'),
  ).toHaveCount(0)

  // No runs at all: the graph reads exactly as it did before the dot existed.
  await page.unrouteAll()
  await stub(page, graph(), { runs: [] })
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')
  await expect(page.locator('main').getByRole('button', { name: /trips_clean/ })).toBeVisible()
  await expect(page.getByTestId('node-health')).toHaveCount(0)
})

test('the selected table opens in the catalog', async ({ page }) => {
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')
  await page.locator('main').getByRole('button', { name: /trips_clean/ }).click()
  const link = page.locator('main').getByRole('link', { name: 'Open table' })
  await expect(link).toHaveAttribute('href', '/catalog?catalog=lakehouse&ns=staging&tbl=trips_clean')
  await link.click()
  await expect(page).toHaveURL(/\/catalog\?catalog=lakehouse&ns=staging&tbl=trips_clean/)
})

test('the panel reserves its width, so the consumer cards are not under it', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await stub(page)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  const node = (name: string) => page.locator('main').getByRole('button', { name: new RegExp(name) })
  await node('trips_clean').click()

  const flow = (await page.locator('.react-flow').boundingBox())!
  const panel = (await page.getByRole('complementary', { name: /trips_clean lineage/ }).boundingBox())!
  // The flow's right edge stops at the panel's left edge: nothing is drawn under the panel.
  expect(flow.x + flow.width).toBeLessThanOrEqual(panel.x + 0.5)

  // Escape closes the panel and the graph gets the width back.
  await page.keyboard.press('Escape')
  await expect(page.getByRole('complementary', { name: /trips_clean lineage/ })).toHaveCount(0)
  const wide = (await page.locator('.react-flow').boundingBox())!
  expect(wide.width).toBeGreaterThan(flow.width + 300)
})

test('the lists mark each table with the state of its last run', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  const runs = {
    runs: [
      { namespace: 'taxi', name: 'load-trips', state: 'success', startedAt: LAST },
      { namespace: 'marts', name: 'revenue', state: 'failed', startedAt: LAST },
    ],
  }
  await stub(page, graph(), runs)
  await page.goto('/lineage?catalog=lakehouse&schema=staging&table=trips_clean')

  const upstream = page.locator('li').filter({ hasText: 'trips_raw' })
  await expect(upstream.getByTestId('list-health')).toHaveAttribute('title', /Last run: succeeded/)
  const downstream = page.locator('li').filter({ hasText: 'daily_revenue' })
  await expect(downstream.getByTestId('list-health')).toHaveAttribute('title', /Last run: failed/)
})
