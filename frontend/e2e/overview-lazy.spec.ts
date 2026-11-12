import { test, expect, type Page, type Route } from '@playwright/test'

// The Overview's performance budget (stardelt-nova#510 item 6, #859): the page keeps its
// answers for 30 s, and the two reads of the Recent activity card — which sits below the
// fold — start only when that card comes near the viewport. The pulse charts above the fold
// read the *same* endpoints with a different `limit`, so every assertion here goes by the
// full URL, not the pathname (as `overview-scope.spec.ts` already does). Every `/api/**`
// call is stubbed, so no cluster is needed.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' }

/** An instance with nothing to browse yet: the Catalogs card ends in "No catalogs to show.". */
const METRICS = {
  catalogs: [],
  totals: { catalogs: 0, schemas: 0, tables: 0, records: 0, sizeBytes: 0 },
  generatedAt: 0,
}

const RUNS = { runs: [], groups: [] }
const QUERY_PAGE = { items: [], nextCursor: null }

/** Every API call the page makes, as `pathname + search`; the query string tells the
 *  timeline's read (`limit=20`, or no `limit` at all) from the charts' (`limit=200`). */
async function stubApi(page: Page) {
  const paths: string[] = []
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    paths.push(url.pathname + url.search)
    const path = url.pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/catalog/metrics') return json(route, METRICS)
    // List endpoints, not `{}`: the shell and the page read them as arrays.
    if (path === '/api/jobs') return json(route, [])
    if (path === '/api/apps/links') return json(route, [])
    // static-dev is the operator here, so the page also renders the Platform apps card.
    if (path === '/api/apps') return json(route, [])
    if (path === '/api/settings') return json(route, {})
    if (path === '/api/catalog/catalogs') return json(route, { catalogs: [], default: '' })
    if (path === '/api/runs') return json(route, RUNS)
    if (path === '/api/query-log') return json(route, QUERY_PAGE)
    return json(route, {})
  })
  return paths
}

/** The `limit` a recorded URL asked with, or `null` when it sent none. */
const limitOf = (p: string) => new URL(p, 'http://n').searchParams.get('limit')

/** The timeline's reads, told from the charts' by their parameters: runs without a `limit`,
 *  the query log with `limit=20` (the charts ask both endpoints with `limit=200`). */
const timelineRuns = (paths: string[]) => paths.filter((p) => new URL(p, 'http://n').pathname === '/api/runs' && limitOf(p) === null)
const timelineQueries = (paths: string[]) => paths.filter((p) => new URL(p, 'http://n').pathname === '/api/query-log' && limitOf(p) === '20')
const chartQueries = (paths: string[]) => paths.filter((p) => new URL(p, 'http://n').pathname === '/api/query-log' && limitOf(p) === '200')

test('the timeline reads nothing before it is scrolled into view', async ({ page }) => {
  const paths = await stubApi(page)
  await page.goto('/')

  // The page has settled with the card above the fold: the Catalogs card is its answer.
  await expect(page.getByText('No catalogs to show.')).toBeVisible()
  // The charts above the fold have read both endpoints — and that is all that was read.
  expect(paths).toContain('/api/runs?limit=200')
  expect(chartQueries(paths)).toHaveLength(1)
  expect(timelineRuns(paths)).toHaveLength(0)
  expect(timelineQueries(paths)).toHaveLength(0)

  // Bringing the card near the viewport starts its two reads.
  await page.getByRole('heading', { name: 'Recent activity' }).scrollIntoViewIfNeeded()
  await expect.poll(() => timelineRuns(paths).length).toBeGreaterThan(0)
  await expect.poll(() => timelineQueries(paths).length).toBeGreaterThan(0)
})

test('coming back inside 30 s re-reads nothing', async ({ page }) => {
  const paths = await stubApi(page)
  await page.goto('/')
  await expect(page.getByText('No catalogs to show.')).toBeVisible()

  const metricsReads = () => paths.filter((p) => p === '/api/catalog/metrics').length
  expect(metricsReads()).toBe(1)

  // In-app navigation, never a second document load: `QueryClient` is created once
  // (`src/main.tsx`), and only its cache can answer inside the 30 s budget.
  await page.getByRole('link', { name: 'Catalog', exact: true }).click()
  await expect(page).toHaveURL(/\/catalog/)
  await page.getByRole('link', { name: 'Overview', exact: true }).click()
  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByText('No catalogs to show.')).toBeVisible()

  // The answer was younger than 30 s, so the page took it from the cache.
  expect(metricsReads()).toBe(1)
})
