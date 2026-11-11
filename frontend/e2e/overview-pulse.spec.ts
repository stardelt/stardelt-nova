import { test, expect, type Page, type Route } from '@playwright/test'

// The Overview's two small charts (stardelt-nova#767, slice 2 of #510): queries per day over
// fourteen days and runs per day over seven, both read from the existing list endpoints.
// Every `/api/**` call is stubbed and the timestamps are computed here, so the window is
// deterministic and no cluster is needed.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' }

const METRICS = {
  catalogs: [],
  totals: { catalogs: 0, schemas: 0, tables: 0, records: 0, sizeBytes: 0 },
  generatedAt: 0,
}

/** Three queries: one today, one two days ago, one three days ago — inside the 14-day window. */
const QUERY_ITEMS = [
  { id: 'q-today', state: 'FINISHED', createdAt: Date.now() - 3_600_000 },
  { id: 'q-2d', state: 'FINISHED', createdAt: Date.now() - 2 * 86_400_000 },
  { id: 'q-3d', state: 'FAILED', createdAt: Date.now() - 3 * 86_400_000 },
].map((q) => ({
  ...q,
  user: 'e2e',
  userName: 'E2E',
  source: 'nova-sql-editor',
  warehouse: 'warehouse',
  queryType: 'SELECT',
  sql: 'select 1',
  wallMs: 12,
  outputRows: 1,
  errorCode: null,
}))

const RUN = (id: string, state: string, minutesAgo: number) => ({
  namespace: 'default',
  name: 'pulse-pipeline',
  ownerGroup: null,
  kind: 'pipeline' as const,
  runId: id,
  state,
  type: 'manual',
  logicalDate: new Date(Date.now() - minutesAgo * 60_000).toISOString(),
  startedAt: new Date(Date.now() - minutesAgo * 60_000).toISOString(),
  endedAt: new Date(Date.now() - minutesAgo * 60_000 + 30_000).toISOString(),
  durationSeconds: 30,
  triggeredBy: 'e2e',
  note: null,
})

/** Two successes and one failure, all started within the last day. */
const RUNS = { runs: [RUN('r1', 'success', 10), RUN('r2', 'success', 20), RUN('r3', 'failed', 30)], groups: [] }

async function stubApi(page: Page, opts: { queryLogStatus?: number; seen?: { from?: string | null } } = {}) {
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/catalog/metrics') return json(route, METRICS)
    if (path === '/api/jobs') return json(route, [])
    if (path === '/api/apps/links') return json(route, [])
    if (path === '/api/apps') return json(route, [])
    if (path === '/api/settings') return json(route, {})
    if (path === '/api/query-log') {
      // The chart asks for a window; the timeline's read sends no `from`.
      if (opts.seen && url.searchParams.get('from') !== null) opts.seen.from = url.searchParams.get('from')
      if (opts.queryLogStatus) return json(route, { error: 'boom' }, opts.queryLogStatus)
      return json(route, { items: QUERY_ITEMS, nextCursor: null })
    }
    if (path === '/api/runs') return json(route, RUNS)
    return json(route, {})
  })
}

/** The card, found by its heading — the timeline card below it says some of the same words. */
const pulseCard = (page: Page) =>
  page.locator('div.card', { has: page.getByRole('heading', { name: 'The last two weeks' }) })

test('the card draws fourteen query days and seven run days', async ({ page }) => {
  const seen: { from?: string | null } = {}
  await stubApi(page, { seen })
  await page.goto('/')
  const card = pulseCard(page)
  await expect(card).toBeVisible()

  const days = card.locator('svg[data-chart="queries"] [data-day]')
  await expect(days).toHaveCount(14)
  const counts = await days.evaluateAll((nodes) => nodes.map((n) => Number(n.getAttribute('data-count'))))
  expect(counts.reduce((a, b) => a + b, 0)).toBe(3)
  // The chart reads a window, so its `from` is an epoch-millisecond number.
  expect(seen.from).toMatch(/^\d+$/)

  const bars = card.locator('svg[data-chart="runs"] [data-day]')
  await expect(bars).toHaveCount(7)
  const ok = await bars.evaluateAll((nodes) => nodes.map((n) => Number(n.getAttribute('data-ok'))))
  const failed = await bars.evaluateAll((nodes) => nodes.map((n) => Number(n.getAttribute('data-failed'))))
  expect(ok.reduce((a, b) => a + b, 0)).toBe(2)
  expect(failed.reduce((a, b) => a + b, 0)).toBe(1)
})

test('the card never prints NaN or an Invalid Date', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  const card = pulseCard(page)
  await expect(card).toBeVisible()
  await expect(card.locator('svg[data-chart="queries"]')).toBeVisible()
  const text = await card.innerText()
  expect(text).not.toContain('NaN')
  expect(text).not.toContain('Invalid Date')
})

test('a failing query log hides only the queries chart', async ({ page }) => {
  await stubApi(page, { queryLogStatus: 500 })
  await page.goto('/')
  const card = pulseCard(page)
  await expect(card).toBeVisible()
  await expect(card.getByText('Could not read queries.')).toBeVisible()
  await expect(card.locator('svg[data-chart="queries"]')).toHaveCount(0)
  await expect(card.locator('svg[data-chart="runs"]')).toBeVisible()
})
