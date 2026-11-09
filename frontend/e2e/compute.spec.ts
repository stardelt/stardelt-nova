import { expect, test } from '@playwright/test'

// The page against the real backend in its no-cluster dev mode (`playwright.config.ts`):
// `state.kube` is None, so `GET /api/compute` answers the one synthetic `shared` row.

test('the Compute page lists the shared cluster and offers no buttons for it', async ({ page }) => {
  await page.goto('/compute')
  const main = page.locator('main')
  await expect(main.getByRole('heading', { name: 'Compute' })).toBeVisible()
  await expect(main.getByText('Shared', { exact: true })).toBeVisible()
  // The page's own sentence also says "always on", so the card's line is the assertion.
  await expect(main.getByText('Shared · always on · for everyone')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Start' })).toHaveCount(0)
})

test('the sidebar links to Compute', async ({ page }) => {
  await page.goto('/')
  await page.locator('#primary-nav').getByRole('link', { name: 'Compute' }).click()
  await expect(page.locator('main').getByRole('heading', { name: 'Compute' })).toBeVisible()
})
