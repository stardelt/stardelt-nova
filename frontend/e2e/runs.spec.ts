import { test, expect, type Page, type Route } from '@playwright/test'

// The Runs page (stardelt-nova#621): every run of the caller's notebooks and pipelines in
// one table, filterable by kind, state and group. Every `/api/**` call is stubbed. The rows
// are flat, exactly as `GET /api/runs` sends them (`#[serde(flatten)]` in
// `backend/src/pipelines.rs`): the run's fields sit beside `namespace`, `name` and `kind`.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
  is_admin: false, groups: ['analytics', 'finance', 'ops'],
}

const SUCCESS = {
  namespace: 'stardelt', name: 'daily_revenue', ownerGroup: 'analytics', kind: 'sql',
  runId: 'scheduled__1', state: 'success', type: 'scheduled', logicalDate: null,
  startedAt: '2026-11-09T09:00:00Z', endedAt: '2026-11-09T09:01:00Z', durationSeconds: 61,
  triggeredBy: 'scheduler', note: null,
}
const FAILED = {
  namespace: 'stardelt', name: 'nightly_upload', ownerGroup: 'finance', kind: 'notebook',
  runId: 'manual__2', state: 'failed', type: 'manual', logicalDate: null,
  startedAt: '2026-11-09T09:05:00Z', endedAt: '2026-11-09T09:06:00Z', durationSeconds: 60,
  triggeredBy: 'alice', note: null,
}
const RUNNING = {
  namespace: 'stardelt', name: 'ingest_orders', ownerGroup: 'mobility', kind: 'pipeline',
  runId: 'scheduled__3', state: 'running', type: 'scheduled', logicalDate: null,
  startedAt: '2026-11-09T09:10:00Z', endedAt: null, durationSeconds: null,
  triggeredBy: 'scheduler', note: null,
}

async function open(page: Page) {
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    if (url.pathname === '/api/me') return json(route, ME)
    if (url.pathname === '/api/runs') return json(route, { runs: [SUCCESS, FAILED, RUNNING] })
    return json(route, [])
  })
  await page.goto('/runs')
  return page
}

const runsRequests = (page: Page, has: (url: URL) => boolean = () => true) =>
  page.waitForRequest((request) => {
    const url = new URL(request.url())
    return url.pathname === '/api/runs' && has(url)
  })

test.describe('runs', () => {
  test('lists the runs of every pipeline with its state, kind and group', async ({ page }) => {
    await open(page)
    const table = page.getByRole('table', { name: /Every run of the notebooks and pipelines/ })
    await expect(table.getByRole('row')).toHaveCount(4)
    await expect(table.getByRole('row', { name: /daily_revenue/ })).toContainText('Succeeded')
    await expect(table.getByRole('row', { name: /nightly_upload/ })).toContainText('Failed')
    await expect(table.getByRole('row', { name: /ingest_orders/ })).toContainText('Running')
    await expect(table.getByRole('row', { name: /daily_revenue/ })).toContainText('SQL')
    await expect(table.getByRole('row', { name: /nightly_upload/ })).toContainText('Notebook')
    await expect(table.getByRole('row', { name: /ingest_orders/ })).toContainText('Pipeline')
    await expect(table.getByRole('row', { name: /daily_revenue/ })).toContainText('analytics')
    await expect(table.getByRole('row', { name: /daily_revenue/ })).toContainText('scheduler')
  })

  test('the kind filter writes the address and asks the API for that kind', async ({ page }) => {
    await open(page)
    const asked = runsRequests(page, (url) => url.searchParams.get('kind') === 'notebook')
    await page.getByLabel('Kind', { exact: true }).selectOption('notebook')
    const request = await asked
    const url = new URL(request.url())
    expect(url.searchParams.get('kind')).toBe('notebook')
    expect(url.searchParams.get('limit')).toBe('100')
    await expect(page).toHaveURL(/[?&]kind=notebook\b/)
  })

  test('the state filter keeps the kind and writes both', async ({ page }) => {
    await open(page)
    await page.getByLabel('Kind', { exact: true }).selectOption('notebook')
    await expect(page).toHaveURL(/[?&]kind=notebook\b/)
    const asked = runsRequests(page, (url) => url.searchParams.get('state') === 'failed')
    await page.getByLabel('State', { exact: true }).selectOption('failed')
    const request = await asked
    const url = new URL(request.url())
    expect(url.searchParams.get('kind')).toBe('notebook')
    expect(url.searchParams.get('state')).toBe('failed')
    await expect(page).toHaveURL(/[?&]kind=notebook\b/)
    await expect(page).toHaveURL(/[?&]state=failed\b/)
  })

  test('the row name opens its pipeline', async ({ page }) => {
    await open(page)
    await page.getByRole('link', { name: 'daily_revenue' }).click()
    await expect(page).toHaveURL('/pipelines/stardelt/daily_revenue')
  })

  test('a running row is asked for again', async ({ page }) => {
    await open(page)
    await expect(page.getByRole('row', { name: /ingest_orders/ })).toContainText('Running')
    const again = page.waitForRequest(
      (request) => new URL(request.url()).pathname === '/api/runs',
      { timeout: 6000 },
    )
    await again
  })
})
