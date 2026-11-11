import { test, expect } from '@playwright/test'

// The public lab's acceptance run — `npm run e2e:public-demo` with `frontend/playwright.live.config.ts`.
// It signs in exactly as a visitor does, as the shared account, and asserts the five checks the
// public demo's acceptance is made of. It needs the live lab and the shared password, so the normal
// `npm run e2e` skips it: no test with a public password may run per push.
test.skip(process.env.PUBLIC_DEMO !== '1', 'live public-demo acceptance; set PUBLIC_DEMO=1')

// The shared account of the public demo. It is not a secret in the configuration sense: every
// visitor is told to sign in with it, and the account can only read the demo catalogs and write
// to `playground`.
const SHARED_USER = 'demo'
const SHARED_PASSWORD = 'demo'

test('a visitor sees the demo catalogs, can query and write, and is closed out of the apps', async ({ page }) => {
  // Sign in as a visitor: the app redirects to the IdP's login form, the same one everyone sees.
  await page.goto('/')
  await page.fill('#username', SHARED_USER)
  await page.fill('#password', SHARED_PASSWORD)
  await page.click('#kc-login')
  await expect(page.getByText('Overview').first()).toBeVisible({ timeout: 60_000 })

  // banner: the shell says the account is shared and its data is deleted nightly. `role="note"`
  // is the banner itself: "Public demo" also appears in the query scope chip (`SQL: Public demo`).
  await expect(page.getByRole('note')).toContainText('Public demo: everyone signs in as this account')

  // catalogs: the four demo catalogs are listed; the other catalogs of this cluster are not.
  await page.goto('/catalog')
  const catalog = page.locator('main')
  for (const name of ['bronze', 'silver', 'gold', 'playground']) {
    await expect(catalog.getByText(name, { exact: true }).first()).toBeVisible({ timeout: 30_000 })
  }
  for (const name of ['mobility', 'retail', 'shared', 'warehouse']) {
    await expect(catalog.getByText(name, { exact: true })).toHaveCount(0)
  }

  // the gold query: a real statement over the shop data answers a number.
  await page.goto('/query')
  const editor = page.getByLabel('SQL query')
  const run = async (sql: string) => {
    await editor.fill(sql)
    await page.getByRole('button', { name: /^Run/ }).click()
  }
  await run('select count(*) from gold.shop.daily_revenue')
  await expect(page.locator('tbody tr')).toHaveCount(1, { timeout: 60_000 })
  await expect(page.locator('tbody tr td').first()).toHaveText(/^\d+$/)

  // playground write: a visitor may create, read and drop a table of their own.
  const table = `playground.scratch.check_${Date.now()}`
  try {
    await run(`create table ${table} as select 1 as x`)
    await expect(page.locator('tbody')).toContainText('1', { timeout: 60_000 })
    await run(`select * from ${table}`)
    await expect(page.locator('tbody')).toContainText('1', { timeout: 60_000 })
  } finally {
    // Always drop it: the nightly reset is small only if acceptance runs clean up after themselves.
    await run(`drop table if exists ${table}`)
    await expect(page.getByText('0 rows')).toBeVisible({ timeout: 60_000 })
  }

  // closed apps: notebooks — the hub has never seen this person, so Start is refused and the
  // workspace stays stopped (the hub's 404 for an unknown user answers `stopped`).
  await page.goto('/notebooks')
  const workspace = page.getByRole('region', { name: 'My workspace' })
  await expect(workspace.getByText('stopped', { exact: true })).toBeVisible({ timeout: 30_000 })
  // Start is the hub's own spawn route, a link that opens in a new tab (nova#808, nova#829).
  // Nova's API start is never offered while the app list is unanswered, so there is no button
  // to click and no alert to read: the spawn lands on the hub's login and the card stays
  // `stopped`.
  const start = workspace.getByRole('link', { name: 'Start' })
  await expect(start).toHaveAttribute('href', /\/hub\/spawn$/)
  const [spawned] = await Promise.all([page.waitForEvent('popup'), start.click()])
  await spawned.close()
  await expect(workspace.getByText('starting', { exact: true })).toHaveCount(0)
  await expect(workspace.getByText('stopped', { exact: true })).toBeVisible()
  await expect(page.getByRole('alert')).toHaveCount(0)

  // closed apps: Superset — Nova renders the embed and lists no dashboard of its own. The
  // cross-origin dashboard list behind the IdP is not asserted here (see stardelt-nova#787).
  await page.goto('/dashboards')
  await expect(page.locator('iframe[title="Superset"]')).toBeVisible({ timeout: 30_000 })
})
