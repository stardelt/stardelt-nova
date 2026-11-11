import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown) =>
  route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['visitors'] }

test('a shared account sees the star and install links under the warning', async ({ page }) => {
  await page.route('**/api/me', (r) => json(r, { ...ME, shared_account: true }))
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Account menu for alice' })).toBeVisible()
  await expect(page.getByRole('link', { name: /Star it on GitHub/ })).toHaveAttribute('href', 'https://github.com/stardelt/stardelt')
  await expect(page.getByRole('link', { name: /Install it yourself/ })).toBeVisible()
  await expect(page.getByRole('note')).toContainText('Public demo:')
})

test('a normal installation sees neither', async ({ page }) => {
  await page.route('**/api/me', (r) => json(r, ME))
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Account menu for alice' })).toBeVisible()
  await expect(page.getByRole('link', { name: /Star it on GitHub/ })).toHaveCount(0)
  await expect(page.getByRole('note')).toHaveCount(0)
})
