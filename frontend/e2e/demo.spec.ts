import { expect, test } from '@playwright/test'

// Demo mode (#500): the pages with samples must render without a red error.
// The server this project points at runs with NOVA_DEMO=1 (playwright.config.ts).
const PAGES = ['/pipelines', '/connections', '/queries', '/streams', '/notebooks']

for (const route of PAGES) {
  test(`demo mode: ${route} shows no red error`, async ({ page }) => {
    await page.goto(route)
    // Let /api/me and the page's own queries settle. `networkidle` alone is too early:
    // `frontend/src/main.tsx` retries a 5xx once after 1 s, so a 503 alert appears
    // ~2 s after the first response (measured on origin/main 2026-11-08: /pipelines
    // alerts=0 at +0 ms, 2 at +2000 ms).
    await page.waitForLoadState('networkidle')
    await page.waitForTimeout(2500)
    // `role=alert` is the error signal. Do not count `.text-bad`: the same class styles
    // destructive controls (the Delete buttons on /connections), not only errors.
    expect(await page.locator('[role=alert]').count(), `${route} has a role=alert`).toBe(0)
  })
}

test('demo mode: the Pipelines page lists the two samples', async ({ page }) => {
  await page.goto('/pipelines')
  await expect(page.getByText('daily_revenue')).toBeVisible()
  await expect(page.getByText('ingest_orders')).toBeVisible()
})

test('demo mode: the Pipelines list says where each sample comes from, and the filter narrows it', async ({ page }) => {
  await page.goto('/pipelines')
  const daily = page.getByRole('row', { name: /daily_revenue/ })
  const orders = page.getByRole('row', { name: /ingest_orders/ })
  // Deployed from the bundle, and one someone changed by hand afterwards (nova#618).
  await expect(daily).toContainText('Git · shop-etl/prod @ abc1234')
  await expect(daily.getByText('changed by hand')).toHaveCount(0)
  await expect(orders).toContainText('Git · shop-etl/prod @ abc1234')
  await expect(orders.getByText('changed by hand')).toBeVisible()
  // The choice is in the address, so a reload keeps it; `replace` keeps Back out of it.
  const filter = page.getByLabel('Source', { exact: true })
  await filter.selectOption('modified')
  await expect(page).toHaveURL(/[?&]source=modified\b/)
  await expect(daily).toHaveCount(0)
  await expect(orders).toBeVisible()
  await page.reload()
  await expect(page.getByLabel('Source', { exact: true })).toHaveValue('modified')
  await expect(daily).toHaveCount(0)
  await page.getByLabel('Source', { exact: true }).selectOption('nova')
  await expect(page.getByText('No pipeline matches this source.')).toBeVisible()
  await page.getByLabel('Source', { exact: true }).selectOption('all')
  await expect(daily).toBeVisible()
  await expect(orders).toBeVisible()
})

test('demo mode: the Connections page offers no write action that would answer 503', async ({ page }) => {
  await page.goto('/connections')
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('button', { name: 'New connection' })).toHaveCount(0)
  expect(await page.getByText('Read-only in demo').count()).toBeGreaterThan(0)
  await page.waitForTimeout(2500)
  expect(await page.locator('[role=alert]').count(), 'connections has a role=alert').toBe(0)
})

test('demo mode: the Pipelines page offers no Schedule action that would answer 503', async ({ page }) => {
  await page.goto('/pipelines')
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('button', { name: 'Schedule a notebook' })).toHaveCount(0)
  await expect(page.getByText('Demo mode shows two sample pipelines.')).toBeVisible()
  await page.waitForTimeout(2500)
  expect(await page.locator('[role=alert]').count(), '/pipelines has a role=alert').toBe(0)
})

test('demo mode: the Query log says why it is empty', async ({ page }) => {
  await page.goto('/queries')
  await expect(page.getByText('Demo mode keeps no query history')).toBeVisible()
})

test('demo mode: the Streams page lists the two samples', async ({ page }) => {
  await page.goto('/streams')
  await expect(page.getByText('pegelonline-live')).toBeVisible()
  await expect(page.getByText('orders-cdc')).toBeVisible()
})

test('demo mode: the Notebooks page says nothing is published yet', async ({ page }) => {
  await page.goto('/notebooks')
  await expect(page.getByText(/no published notebooks/i)).toBeVisible()
})

test('demo mode: a catalog table shows its documentation, no red error', async ({ page }) => {
  await page.goto('/catalog?ns=nyc_taxi&tbl=yellow_trips')
  await page.waitForLoadState('networkidle')
  await expect(page.getByText('NYC yellow-taxi trips, one row per completed trip.')).toBeVisible()
  expect(await page.locator('[role=alert]').count(), '/catalog has a role=alert').toBe(0)
})

test('demo mode: a sample pipeline opens its detail page', async ({ page }) => {
  await page.goto('/pipelines/stardelt/daily_revenue')
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('heading', { name: 'daily_revenue' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Edit schedule' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Delete pipeline' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: /definition/i })).toHaveCount(0)
  await page.waitForTimeout(2500)
  expect(await page.locator('[role=alert]').count(), 'pipeline detail has a role=alert').toBe(0)
})
