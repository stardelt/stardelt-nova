import { test, expect, type Route } from '@playwright/test'

// The search box of the "Published notebooks" list (stardelt-nova#517, item 3): a name narrows the
// list client-side, a search with no hit says so and can be cleared, and the group select still
// narrows on its own. Every `/api/**` call is stubbed, so no cluster is needed.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const NOTEBOOKS = [
  { group: 'analytics', name: 'revenue', objectVersion: 'v1', size: 1000, uploadedAt: '2026-01-01T00:00:00Z', uploadedBy: 'anna' },
  { group: 'analytics', name: 'retention', objectVersion: 'v1', size: 1000, uploadedAt: '2026-01-01T00:00:00Z', uploadedBy: 'anna' },
  { group: 'retail', name: 'orders', objectVersion: 'v1', size: 1000, uploadedAt: '2026-01-01T00:00:00Z', uploadedBy: 'anna' },
]

async function stub(page: import('@playwright/test').Page) {
  await page.route('**/api/**', (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/me') {
      return json(route, { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' })
    }
    if (path === '/api/notebooks') return json(route, NOTEBOOKS)
    if (path === '/api/pipelines/principals') return json(route, { groups: [] })
    if (path === '/api/embeds') return json(route, [])
    if (path === '/api/workspace') return json(route, { state: 'stopped' })
    if (path === '/api/settings') return json(route, {})
    return json(route, {})
  })
}

test('the list shows every published notebook', async ({ page }) => {
  await stub(page)
  await page.goto('/notebooks')
  const section = page.getByRole('region', { name: 'Published notebooks' })
  for (const name of ['revenue', 'retention', 'orders']) {
    await expect(section.getByRole('row', { name: new RegExp(name) })).toBeVisible()
  }
})

test('the search narrows the list to a name', async ({ page }) => {
  await stub(page)
  await page.goto('/notebooks')
  const section = page.getByRole('region', { name: 'Published notebooks' })
  await section.getByLabel('Search', { exact: true }).fill('retail')
  await expect(section.getByRole('row', { name: /orders/ })).toBeVisible()
  await expect(section.getByRole('row', { name: /revenue/ })).toHaveCount(0)
  await expect(section.getByRole('row', { name: /retention/ })).toHaveCount(0)
})

test('a search with no hit says so, and Clear brings the list back', async ({ page }) => {
  await stub(page)
  await page.goto('/notebooks')
  const section = page.getByRole('region', { name: 'Published notebooks' })
  const search = section.getByLabel('Search', { exact: true })
  await search.fill('zzz')
  await expect(section.getByText('No notebook matches')).toBeVisible()
  await expect(section.getByRole('table')).toHaveCount(0)
  await section.getByRole('button', { name: 'Clear' }).click()
  await expect(search).toHaveValue('')
  for (const name of ['revenue', 'retention', 'orders']) {
    await expect(section.getByRole('row', { name: new RegExp(name) })).toBeVisible()
  }
})

test('the group select still narrows the list', async ({ page }) => {
  await stub(page)
  await page.goto('/notebooks')
  const section = page.getByRole('region', { name: 'Published notebooks' })
  await section.getByLabel('Group', { exact: true }).selectOption('analytics')
  await expect(section.getByRole('row', { name: /revenue/ })).toBeVisible()
  await expect(section.getByRole('row', { name: /orders/ })).toHaveCount(0)
})
