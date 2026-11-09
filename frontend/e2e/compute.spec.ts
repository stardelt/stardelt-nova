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

test('an administrator opens the new-cluster dialog and it validates a bad name', async ({ page }) => {
  await page.goto('/compute')
  await page.getByRole('button', { name: 'New cluster' }).click()
  const dialog = page.locator('dialog')
  await expect(dialog.getByRole('heading', { name: 'New cluster' })).toBeVisible()
  await dialog.getByLabel('Name', { exact: true }).fill('Bad Name')
  await expect(dialog.getByText(/use lower-case letters, digits and dashes/)).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Save' })).toBeDisabled()
  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
})

test('the shared card is editable but not deletable', async ({ page }) => {
  await page.goto('/compute')
  await page.getByRole('button', { name: 'Edit' }).click()
  const dialog = page.locator('dialog')
  await expect(dialog.getByRole('heading', { name: 'Edit shared' })).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Delete' })).toHaveCount(0)
  await dialog.getByRole('button', { name: 'Cancel' }).click()
  await expect(dialog).toHaveCount(0)
})
