import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test('no selection asks for a table', async ({ page }) => {
  const lineageRequests: string[] = []
  page.on('request', (r) => {
    if (r.url().includes('/api/lineage')) lineageRequests.push(r.url())
  })
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    if (path === '/api/catalogs') return json(r, [])
    return json(r, [])
  })
  await page.goto('/lineage')
  await expect(page.getByText('Choose a table')).toBeVisible()
  await page.waitForTimeout(300)
  expect(lineageRequests).toEqual([])
})

test('a graph renders jobs and hides an invisible dataset', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    if (path === '/api/catalogs') return json(r, [])
    if (path === '/api/lineage') {
      return json(r, {
        nodes: [
          { id: 'a', namespace: 'warehouse.sales', name: 'orders', visible: true },
          { id: 'b', namespace: 'warehouse.hr', name: 'salaries', visible: false, label: 'a dataset you cannot see' },
        ],
        edges: [{ from: 'a', to: 'b', jobNamespace: 'jobs', jobName: 'nightly_load', runId: 'r1', eventTime: '2026-01-01T00:00:00Z' }],
      })
    }
    return json(r, [])
  })
  await page.goto('/lineage?catalog=warehouse&schema=sales&table=orders')
  await expect(page.getByText('orders', { exact: true })).toBeVisible()
  await expect(page.getByText('a dataset you cannot see', { exact: true })).toBeVisible()
  await expect(page.locator('main').getByText('salaries')).toHaveCount(0)
  await expect(page.locator('svg title', { hasText: 'nightly_load' })).toBeAttached()
})

test('a forbidden table says so', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
    if (path === '/api/catalogs') return json(r, [])
    if (path === '/api/lineage') return json(r, { error: 'forbidden' }, 403)
    return json(r, [])
  })
  await page.goto('/lineage?catalog=warehouse&schema=sales&table=orders')
  await expect(page.locator('main')).toContainText('forbidden')
  await expect(page.locator('main')).not.toContainText('/api/lineage')
})
