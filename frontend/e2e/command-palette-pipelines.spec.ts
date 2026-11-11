import { test, expect, type Route } from '@playwright/test'

// The ⌘K palette finds the person's pipelines and opens one's page
// (stardelt-nova#778, the last slice of the palette design #509). Every `/api/**`
// call is stubbed, so the test needs no cluster.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice',
  auth_mode: 'oidc',
  demo: false,
  warehouse: 'warehouse',
  is_admin: false,
  groups: ['analytics'],
}

const CATALOGS = { catalogs: [{ name: 'lakehouse' }] }

const PIPELINES = {
  pipelines: [
    {
      namespace: 'mobility',
      name: 'kpis_daily',
      ownerGroup: 'mobility',
      schedule: '0 3 * * * (UTC)',
      paused: false,
      loaded: true,
      nextRun: null,
      state: 'running',
      lastRun: null,
      tasks: [],
      runAs: null,
      concurrency: 'Allow',
      createdAt: null,
    },
    {
      namespace: 'shop',
      name: 'orders_hourly',
      ownerGroup: 'shop',
      schedule: null,
      paused: true,
      loaded: true,
      nextRun: null,
      state: 'paused',
      lastRun: null,
      tasks: [],
      runAs: null,
      concurrency: 'Allow',
      createdAt: null,
    },
  ],
}

/**
 * Every API call the shell makes, plus the pipelines list the palette reads. The latter
 * stubs come from `command-palette-queries.spec.ts`; the pipeline page's own calls after
 * navigation fall through to the `{}` catch-all and are not asserted on.
 */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/settings') return json(route, {})
    if (path.startsWith('/api/settings/')) return json(route, { key: path.split('/').pop(), value: null })
    if (path === '/api/catalog/catalogs') return json(route, CATALOGS)
    if (path === '/api/catalog/namespaces') return json(route, { namespaces: [] })
    if (path === '/api/apps/links') return json(route, [])
    // The SQL editor's WarehouseNote reads this as an array (`sql-panes.spec.ts:34`); the
    // catch-all `{}` made the route throw in CI (#614).
    if (path === '/api/warehouses') return json(route, [])
    if (path === '/api/saved-queries') return json(route, [])
    if (path === '/api/query-log') return json(route, { items: [], nextCursor: null })
    if (path === '/api/pipelines') return json(route, PIPELINES)
    return json(route, {})
  })
}

test('the palette finds a pipeline and opens its page', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  const input = page.getByRole('combobox', { name: 'Search pages, queries and tables' })
  // "Pipelines" is also a navigation page, so the group heading is matched on the
  // presentation row that the group label renders as, not on any text in the dialog.
  const pipelineHeading = dialog.locator('li[role="presentation"]').filter({ hasText: /^Pipelines$/ })

  // An empty query shows no pipeline rows, even though the list is loaded.
  await expect(pipelineHeading).toHaveCount(0)

  // A part of the name matches; the hint names the schedule.
  await input.fill('kpis')
  await expect(pipelineHeading).toBeVisible()
  const kpis = dialog.getByRole('option', { name: /mobility\/kpis_daily/ })
  await expect(kpis).toBeVisible()
  await expect(kpis).toContainText('0 3 * * * (UTC)')

  // A paused pipeline says so instead of a schedule, even though it has none.
  await input.fill('orders')
  const orders = dialog.getByRole('option', { name: /shop\/orders_hourly/ })
  await expect(orders).toBeVisible()
  await expect(orders).toContainText('Paused')

  // A query that matches nothing shows no pipeline group and the palette's empty state.
  await input.fill('zzzz')
  await expect(pipelineHeading).toHaveCount(0)
  await expect(dialog.getByText(/Nothing matches/)).toBeVisible()

  // The row opens the pipeline's page.
  await input.fill('kpis')
  await expect(dialog.getByRole('option', { name: /mobility\/kpis_daily/ })).toBeVisible()
  await dialog.getByRole('option', { name: /mobility\/kpis_daily/ }).click()
  await expect(page).toHaveURL(/\/pipelines\/mobility\/kpis_daily$/)
  await expect(dialog).toHaveCount(0)
})
