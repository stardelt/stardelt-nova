import { test, expect, type Page, type Route } from '@playwright/test'

// The three list pages reserve their height with skeleton rows while the list is in flight,
// instead of printing a loading sentence (stardelt-nova#798, slice 1 of #512). Every `/api/**`
// call is stubbed, so the tests need no cluster.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse', groups: [] }

/** One handler for every call: the list answer is 500 ms late so the skeleton is observable. */
async function stubApi(page: Page, delayed: string, body: unknown) {
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === delayed) {
      await new Promise((resolve) => setTimeout(resolve, 500))
      return json(route, body)
    }
    // The header's app switcher reads an array; `{}` would break the shell.
    if (path === '/api/apps/links') return json(route, [])
    if (path === '/api/service-principals') return json(route, { principals: [] })
    return json(route, {})
  })
}

test('/connections shows five skeleton rows, then the table', async ({ page }) => {
  await stubApi(page, '/api/connections', { connections: [] })
  await page.goto('/connections')
  const skeletons = page.locator('.skeleton')
  await expect(skeletons).toHaveCount(5)
  await expect(
    page.getByRole('table', { name: 'Personal connections' }).getByText('Nothing here yet.'),
  ).toBeVisible()
  await expect(skeletons).toHaveCount(0)
})

test('/pipelines shows five skeleton rows, then the table', async ({ page }) => {
  await stubApi(page, '/api/pipelines', { pipelines: [] })
  await page.goto('/pipelines')
  const skeletons = page.locator('.skeleton')
  await expect(skeletons).toHaveCount(5)
  await expect(page.getByText('No pipelines yet')).toBeVisible()
  await expect(skeletons).toHaveCount(0)
})

test('/deployments shows five skeleton rows, then the cards', async ({ page }) => {
  await stubApi(page, '/api/pipelines', { pipelines: [] })
  await page.goto('/deployments')
  const skeletons = page.locator('.skeleton')
  await expect(skeletons).toHaveCount(5)
  await expect(page.getByText('Nothing was deployed from a bundle yet.')).toBeVisible()
  await expect(skeletons).toHaveCount(0)
})
