import { test, expect, type Route, type Page } from '@playwright/test'

// The Overview's Me / My groups switch (stardelt-nova#510 slice 3, #771): "My groups" is the
// first view and asks the query log for `scope=team`; "Mine" asks for `scope=mine` and hides
// the pipeline runs, which belong to a group rather than to a person. Runs in the `demo`
// project (playwright.config.ts), so the run rows come from the demo seed.

const AT = (iso: string) => Date.parse(iso)

const QUERY_ITEMS = [
  {
    id: 'q-scope',
    state: 'FINISHED',
    user: 'demo',
    userName: 'Demo',
    source: 'nova-sql-editor',
    warehouse: 'warehouse',
    queryType: 'SELECT',
    sql: 'select payment_type, count(*) from stardelt.taxi.trips group by 1',
    createdAt: AT('2026-11-08T10:00:00Z'),
    wallMs: 412,
    outputRows: 4,
    errorCode: null,
  },
]

const json = (route: Route, body: unknown) =>
  route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })

/** The card's timeline asks `limit=20`; the pulse card asks `limit=200` and mounts last, so
 *  only the timeline's scope is recorded here. */
async function open(page: Page) {
  const scopes: string[] = []
  await page.route('**/api/query-log**', (route) => {
    const url = new URL(route.request().url())
    if (url.pathname === '/api/query-log' && url.searchParams.get('limit') === '20') {
      scopes.push(url.searchParams.get('scope') ?? '')
    }
    return json(route, { items: QUERY_ITEMS, nextCursor: null })
  })
  await page.goto('/')
  const card = page.locator('div.card', { has: page.getByRole('heading', { name: 'Recent activity' }) })
  await expect(card.locator('a[data-at]').first()).toBeVisible()
  return { card, scopes }
}

test.describe('overview scope switch', () => {
  test('the switch narrows the card to the caller and says what belongs to a group', async ({ page }) => {
    const { card, scopes } = await open(page)

    // "My groups" is the default, and it is the answer the card already showed.
    await expect(card.getByRole('button', { name: 'My groups' })).toHaveAttribute('aria-pressed', 'true')
    await expect.poll(() => scopes[scopes.length - 1]).toBe('team')
    await expect(card.locator('a[href^="/pipelines/"]').first()).toBeVisible()

    await card.getByRole('button', { name: 'Mine', exact: true }).click()
    await expect(card.getByRole('button', { name: 'Mine', exact: true })).toHaveAttribute('aria-pressed', 'true')
    await expect.poll(() => scopes[scopes.length - 1]).toBe('mine')

    // A pipeline run belongs to a group: the narrow view has none, and keeps the queries.
    await expect(card.locator('a[href^="/pipelines/"]')).toHaveCount(0)
    await expect(card.locator('a[href^="/queries?id="]').first()).toBeVisible()

    // The card says why, instead of pretending loads follow the same rule.
    await expect(card.getByText('Loads are always yours.')).toBeVisible()
  })
})
