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

/** One query row, `ageMs` before now — inside the 14-day window. */
const queryItem = (id: string, state: string, ageMs: number) => ({
  id,
  state,
  createdAt: Date.now() - ageMs,
  user: 'e2e',
  userName: 'E2E',
  source: 'nova-sql-editor',
  warehouse: 'warehouse',
  queryType: 'SELECT',
  sql: 'select 1',
  wallMs: 12,
  outputRows: 1,
  errorCode: null,
})

/** Three queries: one today, one two days ago, one three days ago — inside the 14-day window. */
const QUERY_ITEMS = [
  queryItem('q-today', 'FINISHED', 3_600_000),
  queryItem('q-2d', 'FINISHED', 2 * 86_400_000),
  queryItem('q-3d', 'FAILED', 3 * 86_400_000),
]

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

async function stubApi(page: Page, opts: {
  queryLogStatus?: number
  seen?: { from?: string | null }
  /** Answers for the chart's windowed reads, keyed by `cursor` (`''` for the first page). */
  queryPages?: Record<string, { items: unknown[]; nextCursor: string | null }>
  /** Every windowed read the chart makes, in order. */
  calls?: { cursor: string | null }[]
} = {}) {
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
      const cursor = url.searchParams.get('cursor')
      // The chart asks for a window; the timeline's read sends no `from`.
      if (url.searchParams.get('from') !== null) {
        if (opts.seen) opts.seen.from = url.searchParams.get('from')
        if (opts.calls) opts.calls.push({ cursor })
      }
      if (opts.queryLogStatus) return json(route, { error: 'boom' }, opts.queryLogStatus)
      if (opts.queryPages) return json(route, opts.queryPages[cursor ?? ''] ?? { items: [], nextCursor: null })
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

test('the queries chart reads every page of its window', async ({ page }) => {
  // Five queries over two pages (nova#883): the chart follows `nextCursor` instead of
  // drawing the first 200 rows — or, here, the first three — as the whole window.
  const calls: { cursor: string | null }[] = []
  await stubApi(page, {
    calls,
    queryPages: {
      '': {
        items: [
          queryItem('p-1-today', 'FINISHED', 3_600_000),
          queryItem('p-1-2d', 'FINISHED', 2 * 86_400_000),
          queryItem('p-1-3d', 'FAILED', 3 * 86_400_000),
        ],
        nextCursor: 'page-2',
      },
      'page-2': {
        items: [
          queryItem('p-2-4d', 'FINISHED', 4 * 86_400_000),
          queryItem('p-2-5d', 'FINISHED', 5 * 86_400_000),
        ],
        nextCursor: null,
      },
    },
  })
  await page.goto('/')
  const card = pulseCard(page)
  await expect(card).toBeVisible()

  const days = card.locator('svg[data-chart="queries"] [data-day]')
  await expect(days).toHaveCount(14)
  await expect
    .poll(async () => (await days.evaluateAll((nodes) => nodes.map((n) => Number(n.getAttribute('data-count')))))
      .reduce((a, b) => a + b, 0))
    .toBe(5)

  await expect.poll(() => calls.length).toBe(2)
  expect(calls[0].cursor).toBeNull()
  expect(calls[1].cursor).toBe('page-2')
  await expect(card.getByText('Showing the latest 200 queries.')).toHaveCount(0)
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
