import { test, expect, type Route } from '@playwright/test'

// The Connections Personal card reserves the height of its table while the list is in
// flight (stardelt-nova#798, #837). The API is stubbed, so the test needs no cluster.

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: '' }

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test('a connections skeleton row is as tall as a connections row (nova#837)', async ({ page }) => {
  await page.route('**/api/me', (route) => json(route, ME))
  await page.route('**/api/connections', async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 500))
    return json(route, { connections: [] })
  })
  await page.goto('/connections')
  const first = page.locator('.skeleton').first()
  await expect(first).toBeVisible()
  const box = await first.boundingBox()
  expect(box?.height).toBeCloseTo(50, 0)
  await expect(page.locator('.skeleton')).toHaveCount(0)
  // The static-dev user also sees a Managed table with the same empty sentence; scope by table.
  await expect(page.getByRole('table', { name: 'Personal connections' }).getByText('Nothing here yet.')).toBeVisible()
})
